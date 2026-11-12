//! The container registry (ADR-0022), driven over HTTP the way docker,
//! podman and crane do it: token flow, blob uploads (whole and in chunks),
//! manifests and indexes, tags, mounts, access rules, CI credentials, cleanup
//! and garbage collection.

mod common;

use common::*;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const IMAGE: &str = "acme/platform/api";

fn digest(b: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(b)))
}

/// A registry client with a token for `scope`.
struct Reg {
    base: String,
    token: String,
    http: reqwest::Client,
}

async fn token(base: &str, auth: Option<(&str, &str)>, scope: &str) -> Value {
    let mut r = reqwest::Client::new().get(format!("{base}/jwt/auth")).query(&[("service", "container_registry"), ("scope", scope)]);
    if let Some((u, p)) = auth {
        r = r.basic_auth(u, Some(p));
    }
    let resp = r.send().await.unwrap();
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

/// The actions a token grants on `name`.
fn granted(t: &Value, name: &str) -> Vec<String> {
    use base64::Engine;
    let body = t["token"].as_str().unwrap().split('.').nth(1).unwrap();
    let claims: Value = serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body).unwrap()).unwrap();
    claims["access"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["name"] == name)
        .flat_map(|a| a["actions"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()))
        .collect()
}

impl Reg {
    async fn login(base: &str, auth: Option<(&str, &str)>, name: &str) -> Reg {
        let t = token(base, auth, &format!("repository:{name}:pull,push,delete")).await;
        Reg { base: base.into(), token: t["token"].as_str().unwrap().into(), http: reqwest::Client::new() }
    }
    fn req(&self, m: Method, path: &str) -> reqwest::RequestBuilder {
        let url = if path.starts_with("http") { path.to_string() } else { format!("{}{path}", self.base) };
        self.http.request(m, url).bearer_auth(&self.token)
    }
    /// Push a blob in one request.
    async fn blob(&self, name: &str, data: &[u8]) -> (StatusCode, String) {
        let d = digest(data);
        let r = self.req(Method::POST, &format!("/v2/{name}/blobs/uploads/?digest={d}")).body(data.to_vec()).send().await.unwrap();
        (r.status(), d)
    }
    async fn put_manifest(&self, name: &str, reference: &str, media_type: &str, m: &Value) -> reqwest::Response {
        self.req(Method::PUT, &format!("/v2/{name}/manifests/{reference}"))
            .header("content-type", media_type)
            .body(serde_json::to_vec(m).unwrap())
            .send()
            .await
            .unwrap()
    }
}

const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";

fn image_manifest(config: &(String, usize), layers: &[(String, usize)]) -> Value {
    json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST,
        "config": { "mediaType": "application/vnd.oci.image.config.v1+json", "digest": config.0, "size": config.1 },
        "layers": layers.iter().map(|(d, s)| json!({ "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": d, "size": s })).collect::<Vec<_>>(),
    })
}

async fn errors_code(r: reqwest::Response) -> String {
    let v: Value = r.json().await.unwrap_or(Value::Null);
    v["errors"][0]["code"].as_str().unwrap_or("").to_string()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn push_and_pull_an_image(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;

    // the challenge points at the token endpoint
    let r = reqwest::get(format!("{}/v2/", srv.base)).await.unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.headers()["docker-distribution-api-version"], "registry/2.0");
    let challenge = r.headers()["www-authenticate"].to_str().unwrap().to_string();
    assert_eq!(challenge, format!("Bearer realm=\"{}/jwt/auth\",service=\"container_registry\"", srv.base));

    let pw = password("olivia");
    let t = token(&srv.base, Some(("olivia", &pw)), &format!("repository:{IMAGE}:pull,push")).await;
    assert_eq!(granted(&t, IMAGE), ["pull", "push"]);
    let reg = Reg::login(&srv.base, Some(("olivia", &pw)), IMAGE).await;
    assert_eq!(reg.req(Method::GET, "/v2/").send().await.unwrap().status(), 200);

    // a layer in one request; a config in chunks
    let layer = b"layer bytes, pretend this is a tar.gz".to_vec();
    let (s, layer_d) = reg.blob(IMAGE, &layer).await;
    assert_eq!(s, 201);
    let config = br#"{"architecture":"amd64","os":"linux","rootfs":{"type":"layers","diff_ids":[]}}"#.to_vec();
    let r = reg.req(Method::POST, &format!("/v2/{IMAGE}/blobs/uploads/")).send().await.unwrap();
    assert_eq!(r.status(), 202);
    let mut at = format!("{}{}", srv.base, r.headers()["location"].to_str().unwrap());
    let (a, b) = config.split_at(20);
    for (start, part) in [(0, a), (20, b)] {
        let r = reg
            .req(Method::PATCH, &at)
            .header("content-range", format!("{start}-{}", start + part.len() - 1))
            .body(part.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 202);
        assert_eq!(r.headers()["range"], format!("0-{}", start + part.len() - 1));
        at = format!("{}{}", srv.base, r.headers()["location"].to_str().unwrap());
    }
    // a chunk at the wrong offset is refused
    let r = reg.req(Method::PATCH, &at).header("content-range", "5-9").body(b"xxxxx".to_vec()).send().await.unwrap();
    assert_eq!(r.status(), 416);
    let config_d = digest(&config);
    let sep = if at.contains('?') { '&' } else { '?' };
    let r = reg.req(Method::PUT, &format!("{at}{sep}digest={config_d}")).send().await.unwrap();
    assert_eq!(r.status(), 201, "{:?}", r.text().await);
    // a wrong digest is refused
    let r = reg
        .req(Method::POST, &format!("/v2/{IMAGE}/blobs/uploads/?digest={}", digest(b"other")))
        .body(b"not other".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!((r.status(), errors_code(r).await), (StatusCode::BAD_REQUEST, "DIGEST_INVALID".into()));

    // blobs read back
    let r = reg.req(Method::HEAD, &format!("/v2/{IMAGE}/blobs/{layer_d}")).send().await.unwrap();
    assert_eq!((r.status(), r.headers()["content-length"].to_str().unwrap()), (StatusCode::OK, layer.len().to_string().as_str()));
    let r = reg.req(Method::GET, &format!("/v2/{IMAGE}/blobs/{config_d}")).send().await.unwrap();
    assert_eq!(r.bytes().await.unwrap().as_ref(), &config[..]);

    // a manifest that names a blob the repository does not have
    let bad = image_manifest(&(config_d.clone(), config.len()), &[(digest(b"missing"), 7)]);
    let r = reg.put_manifest(IMAGE, "broken", OCI_MANIFEST, &bad).await;
    assert_eq!((r.status(), errors_code(r).await), (StatusCode::BAD_REQUEST, "MANIFEST_BLOB_UNKNOWN".into()));
    // the real one, tagged
    let m = image_manifest(&(config_d.clone(), config.len()), &[(layer_d.clone(), layer.len())]);
    let r = reg.put_manifest(IMAGE, "v1", OCI_MANIFEST, &m).await;
    assert_eq!(r.status(), 201);
    let m_d = r.headers()["docker-content-digest"].to_str().unwrap().to_string();
    assert_eq!(m_d, digest(&serde_json::to_vec(&m).unwrap()));
    let r = reg.req(Method::GET, &format!("/v2/{IMAGE}/manifests/v1")).header("accept", OCI_MANIFEST).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], OCI_MANIFEST);
    assert_eq!(r.headers()["docker-content-digest"], m_d.as_str());
    assert_eq!(r.json::<Value>().await.unwrap(), m);
    assert_eq!(reg.req(Method::HEAD, &format!("/v2/{IMAGE}/manifests/{m_d}")).send().await.unwrap().status(), 200);

    // an index over it (multi-arch), and more tags
    let index = json!({ "schemaVersion": 2, "mediaType": OCI_INDEX, "manifests": [
        { "mediaType": OCI_MANIFEST, "digest": m_d, "size": 1, "platform": { "architecture": "amd64", "os": "linux" } }
    ] });
    assert_eq!(reg.put_manifest(IMAGE, "multi", OCI_INDEX, &index).await.status(), 201);
    let missing = json!({ "schemaVersion": 2, "mediaType": OCI_INDEX, "manifests": [{ "mediaType": OCI_MANIFEST, "digest": digest(b"x"), "size": 1 }] });
    let r = reg.put_manifest(IMAGE, "nope", OCI_INDEX, &missing).await;
    assert_eq!((r.status(), errors_code(r).await), (StatusCode::BAD_REQUEST, "MANIFEST_UNKNOWN".into()));
    assert_eq!(reg.put_manifest(IMAGE, "latest", OCI_MANIFEST, &m).await.status(), 201);
    let tags: Value = reg.req(Method::GET, &format!("/v2/{IMAGE}/tags/list")).send().await.unwrap().json().await.unwrap();
    assert_eq!(tags, json!({ "name": IMAGE, "tags": ["latest", "multi", "v1"] }));
    let r = reg.req(Method::GET, &format!("/v2/{IMAGE}/tags/list?n=1")).send().await.unwrap();
    assert!(r.headers()["link"].to_str().unwrap().contains("last=latest"));

    // a sub-image of the project
    let sub = format!("{IMAGE}/tools/ci");
    let sreg = Reg::login(&srv.base, Some(("olivia", &pw)), &sub).await;
    assert_eq!(sreg.blob(&sub, &layer).await.0, 201);

    // what people see
    let info = owner.ok_get(&format!("/api/v1/projects/{project}/registry")).await;
    let host = srv.host();
    assert_eq!(info["image"], format!("{host}/{IMAGE}"));
    let repos = info["repositories"].as_array().unwrap();
    assert_eq!(repos.len(), 2);
    assert_eq!((repos[0]["name"].as_str(), repos[0]["tags_count"].as_i64()), (Some(""), Some(3)));
    assert_eq!(repos[1]["location"], format!("{host}/{sub}"));
    let rid = repos[0]["id"].as_i64().unwrap();
    let list = owner.ok_get(&format!("/api/v1/projects/{project}/registry/repositories/{rid}/tags")).await;
    assert_eq!(list.as_array().unwrap().len(), 3);
    // GitLab's API, as tools that prune images read it
    let tok = owner.ok_post("/api/v1/user/tokens", json!({ "name": "v4", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let v4 = Client::with_token(&srv.base, &tok);
    let r4 = v4.ok_get(&format!("/api/v4/projects/{project}/registry/repositories?tags=true&tags_count=true")).await;
    assert_eq!((r4[0]["path"].as_str(), r4[0]["location"].as_str()), (Some(IMAGE), Some(format!("{host}/{IMAGE}").as_str())));
    assert_eq!(
        (r4[0]["tags_count"].as_i64(), r4[0]["tags"][0]["location"].as_str()),
        (Some(3), Some(format!("{host}/{IMAGE}:latest").as_str()))
    );
    assert_eq!(r4[1]["path"], sub.as_str());
    let t4 = v4.ok_get(&format!("/api/v4/projects/{project}/registry/repositories/{rid}/tags/v1")).await;
    let v1_tag = list.as_array().unwrap().iter().find(|t| t["name"] == "v1").unwrap();
    assert_eq!((t4["digest"].as_str(), &t4["total_size"]), (Some(m_d.as_str()), &v1_tag["total_size"]), "{t4}");
    assert_eq!(v4.get(&format!("/api/v4/projects/{project}/registry/repositories/{rid}/tags/nope")).await.0, 404);
    assert_eq!(owner.delete(&format!("/api/v1/projects/{project}/registry/repositories/{rid}/tags/latest")).await.0, 200);
    assert_eq!(reg.req(Method::GET, &format!("/v2/{IMAGE}/manifests/latest")).send().await.unwrap().status(), 404);
    // deleting by tag and by digest over the API
    assert_eq!(reg.req(Method::DELETE, &format!("/v2/{IMAGE}/manifests/multi")).send().await.unwrap().status(), 202);
    assert_eq!(reg.req(Method::DELETE, &format!("/v2/{IMAGE}/manifests/{m_d}")).send().await.unwrap().status(), 202);
    assert_eq!(reg.req(Method::GET, &format!("/v2/{IMAGE}/manifests/v1")).send().await.unwrap().status(), 404, "its tags went with it");
    let storage = owner.ok_get(&format!("/api/v1/projects/{project}/storage")).await;
    assert_eq!(storage["registry_bytes"], (layer.len() + config.len()) as i64);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn who_may_pull_push_and_mount(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    for (u, role) in [("rita", "reporter"), ("dave", "developer")] {
        srv.user(u, false).await;
        owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "username": u, "access_level": role })).await;
    }
    srv.user("eve", false).await;
    let (pw, rita, dave, eve) = (password("olivia"), password("rita"), password("dave"), password("eve"));
    let scope = format!("repository:{IMAGE}:pull,push");
    assert_eq!(granted(&token(&srv.base, Some(("rita", &rita)), &scope).await, IMAGE), ["pull"]);
    assert_eq!(granted(&token(&srv.base, Some(("dave", &dave)), &scope).await, IMAGE), ["pull", "push"]);
    assert!(granted(&token(&srv.base, Some(("eve", &eve)), &scope).await, IMAGE).is_empty());
    assert!(granted(&token(&srv.base, None, &scope).await, IMAGE).is_empty(), "a private project: nothing anonymous");
    // bad credentials are refused outright
    let r = reqwest::Client::new()
        .get(format!("{}/jwt/auth?service=container_registry&scope={scope}", srv.base))
        .basic_auth("olivia", Some("wrong"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    let reg = Reg::login(&srv.base, Some(("olivia", &pw)), IMAGE).await;
    let secret_layer = b"private layer".to_vec();
    let (_, d) = reg.blob(IMAGE, &secret_layer).await;
    // a reporter pulls but cannot push
    let rreg = Reg::login(&srv.base, Some(("rita", &rita)), IMAGE).await;
    assert_eq!(rreg.req(Method::GET, &format!("/v2/{IMAGE}/blobs/{d}")).send().await.unwrap().status(), 200);
    let r = rreg.req(Method::POST, &format!("/v2/{IMAGE}/blobs/uploads/")).send().await.unwrap();
    assert_eq!((r.status(), errors_code(r).await), (StatusCode::FORBIDDEN, "DENIED".into()));
    // an outsider is asked to sign in, and learns nothing
    let ereg = Reg::login(&srv.base, Some(("eve", &eve)), IMAGE).await;
    let r = ereg.req(Method::GET, &format!("/v2/{IMAGE}/blobs/{d}")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert!(r.headers()["www-authenticate"].to_str().unwrap().contains(&format!("scope=\"repository:{IMAGE}:pull\"")));

    // eve's own project cannot read olivia's layer by its digest...
    let eve_c = {
        let mut c = Client::new(&srv.base);
        c.login("eve", &eve).await;
        c
    };
    let g = eve_c.ok_post("/api/v1/groups", json!({ "name": "Eve", "path": "eveg" })).await;
    eve_c.ok_post("/api/v1/projects", json!({ "name": "Mine", "path": "mine", "namespace_id": g["id"] })).await;
    let mine = Reg::login(&srv.base, Some(("eve", &eve)), "eveg/mine").await;
    assert_eq!(mine.req(Method::HEAD, &format!("/v2/eveg/mine/blobs/{d}")).send().await.unwrap().status(), 404);
    // ...nor mount it from a repository she cannot read: she gets an upload instead
    let r = mine.req(Method::POST, &format!("/v2/eveg/mine/blobs/uploads/?mount={d}&from={IMAGE}")).send().await.unwrap();
    assert_eq!(r.status(), 202);
    // someone who reads both mounts it
    let g2 = owner.ok_post("/api/v1/groups", json!({ "name": "Other", "path": "other" })).await;
    owner.ok_post("/api/v1/projects", json!({ "name": "Two", "path": "two", "namespace_id": g2["id"] })).await;
    let two_t = token(&srv.base, Some(("olivia", &pw)), &format!("repository:other/two:pull,push repository:{IMAGE}:pull")).await;
    let two = Reg { base: srv.base.clone(), token: two_t["token"].as_str().unwrap().into(), http: reqwest::Client::new() };
    let r = two.req(Method::POST, &format!("/v2/other/two/blobs/uploads/?mount={d}&from={IMAGE}")).send().await.unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(two.req(Method::HEAD, &format!("/v2/other/two/blobs/{d}")).send().await.unwrap().status(), 200);

    // a public project: anyone pulls
    let pg = owner.ok_post("/api/v1/groups", json!({ "name": "Open", "path": "open", "visibility": "public" })).await;
    owner.ok_post("/api/v1/projects", json!({ "name": "Site", "path": "site", "namespace_id": pg["id"], "visibility": "public" })).await;
    assert_eq!(granted(&token(&srv.base, None, "repository:open/site:pull,push").await, "open/site"), ["pull"]);
    // deploy tokens need a registry scope
    let dt =
        owner.ok_post(&format!("/api/v1/projects/{project}/deploy_tokens"), json!({ "name": "ci", "scopes": ["read_registry"] })).await;
    let dt_t = token(&srv.base, Some((dt["username"].as_str().unwrap(), dt["token"].as_str().unwrap())), &scope).await;
    assert_eq!(granted(&dt_t, IMAGE), ["pull"]);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn ci_jobs_log_in_with_their_token(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let work = srv.work_dir("ci");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(work.join(".gitlab-ci.yml"), "build:\n  script: [docker build .]\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "ci"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", IMAGE), "main"]).await);
    wait_jobs(&srv.state).await;
    let runner = owner.ok_post(&format!("/api/v1/projects/{project}/runners"), json!({ "description": "r" })).await;
    let job: Value = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/request", srv.base))
        .json(&json!({ "token": runner["token"], "system_id": "s_reg", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let var =
        |k: &str| job["variables"].as_array().unwrap().iter().find(|v| v["key"] == k).map(|v| v["value"].as_str().unwrap().to_string());
    let host = srv.host();
    assert_eq!(var("CI_REGISTRY").as_deref(), Some(host.as_str()));
    assert_eq!(var("CI_REGISTRY_IMAGE"), Some(format!("{host}/{IMAGE}")));
    assert_eq!(var("CI_REGISTRY_USER").as_deref(), Some("gitlab-ci-token"));
    let job_token = job["token"].as_str().unwrap();
    assert_eq!(var("CI_REGISTRY_PASSWORD").as_deref(), Some(job_token));
    assert_eq!(job["credentials"], json!([{ "type": "registry", "url": host, "username": "gitlab-ci-token", "password": job_token }]));
    // the job pushes to its own project, and nowhere else
    let t = token(&srv.base, Some(("gitlab-ci-token", job_token)), &format!("repository:{IMAGE}:pull,push")).await;
    assert_eq!(granted(&t, IMAGE), ["pull", "push"]);
    let reg = Reg::login(&srv.base, Some(("gitlab-ci-token", job_token)), IMAGE).await;
    assert_eq!(reg.blob(IMAGE, b"built in ci").await.0, 201);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn cleanup_policies_and_garbage_collection(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let pw = password("olivia");
    let reg = Reg::login(&srv.base, Some(("olivia", &pw)), IMAGE).await;
    let config = b"{}".to_vec();
    let (_, config_d) = reg.blob(IMAGE, &config).await;
    for (i, tag) in ["v1", "v2", "v3", "stable"].iter().enumerate() {
        let layer = format!("layer {i}").into_bytes();
        let (_, d) = reg.blob(IMAGE, &layer).await;
        let m = image_manifest(&(config_d.clone(), config.len()), &[(d, layer.len())]);
        assert_eq!(reg.put_manifest(IMAGE, tag, OCI_MANIFEST, &m).await.status(), 201);
        // tags ordered by age
        sqlx::query("UPDATE registry_tags SET updated_at = now() - make_interval(days => $1) WHERE name = $2")
            .bind(100 - i as i32)
            .bind(*tag)
            .execute(&srv.state.db)
            .await
            .unwrap();
    }
    let policy = format!("/api/v1/projects/{project}/registry/cleanup_policy");
    assert_eq!(
        owner
            .put(&policy, json!({ "enabled": true, "keep_n": 1, "older_than_days": 30, "name_regex": "(", "name_regex_keep": "" }))
            .await
            .0,
        400
    );
    let (s, body) = owner
        .put(&policy, json!({ "enabled": true, "keep_n": 1, "older_than_days": 30, "name_regex": "v.*", "name_regex_keep": "stable" }))
        .await;
    assert_eq!(s, 200, "{body}");
    let removed = owner.ok_post(&format!("/api/v1/projects/{project}/registry/cleanup"), json!({})).await;
    assert_eq!(removed["removed"], 2, "v1 and v2 go: v3 is the newest v* tag, stable is always kept");
    let tags: Value = reg.req(Method::GET, &format!("/v2/{IMAGE}/tags/list")).send().await.unwrap().json().await.unwrap();
    assert_eq!(tags["tags"], json!(["stable", "v3"]));

    // GC: untagged manifests, then layers nothing uses (old enough)
    for q in [
        "UPDATE registry_manifests SET created_at = now() - interval '2 days'",
        "UPDATE registry_repository_blobs SET created_at = now() - interval '2 days'",
        "UPDATE registry_blobs SET created_at = now() - interval '2 days'",
    ] {
        sqlx::query(q).execute(&srv.state.db).await.unwrap();
    }
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM registry_blobs").fetch_one(&srv.state.db).await.unwrap();
    forgust_server::registry::sweep(&srv.state).await.unwrap();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM registry_blobs").fetch_one(&srv.state.db).await.unwrap();
    assert_eq!((before, after), (5, 3), "the config and the layers of v3 and stable stay");
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM blob_deletions WHERE blob_key LIKE 'registry/blobs/sha256/%'")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(queued, 2);
    // what is left still pulls
    assert_eq!(reg.req(Method::GET, &format!("/v2/{IMAGE}/manifests/stable")).send().await.unwrap().status(), 200);
}

/// A real client: skopeo (containers/image, as podman and buildah use) copies
/// a multi-arch image from Docker Hub into Forgust and reads it back. Opt in
/// with `FORGUST_TEST_SKOPEO=quay.io/skopeo/stable:<version>` (needs Docker and
/// Docker Hub); `FORGUST_TEST_RUNNER_NETWORK` as for the runner tests.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn real_skopeo_copies_a_multi_arch_image(pool: sqlx::PgPool) {
    let Ok(image) = std::env::var("FORGUST_TEST_SKOPEO") else {
        eprintln!("skipped: set FORGUST_TEST_SKOPEO=quay.io/skopeo/stable:<version> to run the real skopeo");
        return;
    };
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let network = std::env::var("FORGUST_TEST_RUNNER_NETWORK").unwrap_or_else(|_| "host".into());
    let creds = format!("olivia:{}", password("olivia"));
    let dest = format!("docker://{}/{IMAGE}:alpine", srv.host());
    let skopeo = |args: Vec<String>| {
        let (image, network) = (image.clone(), network.clone());
        async move {
            let out = tokio::process::Command::new("docker")
                .args(["run", "--rm", "--network", &network, &image])
                .args(&args)
                .output()
                .await
                .unwrap();
            let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            assert!(out.status.success(), "skopeo {}:\n{text}", args.join(" "));
            text
        }
    };
    let s = |x: &str| x.to_string();
    skopeo(vec![
        s("copy"),
        s("--all"),
        s("--dest-tls-verify=false"),
        format!("--dest-creds={creds}"),
        s("docker://docker.io/library/alpine:3.20"),
        dest.clone(),
    ])
    .await;
    let tags =
        skopeo(vec![s("list-tags"), s("--tls-verify=false"), format!("--creds={creds}"), format!("docker://{}/{IMAGE}", srv.host())]).await;
    assert!(tags.contains("\"alpine\""), "{tags}");
    let raw = skopeo(vec![s("inspect"), s("--raw"), s("--tls-verify=false"), format!("--creds={creds}"), dest.clone()]).await;
    let index: Value = serde_json::from_str(raw.trim()).unwrap();
    assert!(index["manifests"].as_array().unwrap().len() > 1, "a multi-arch index: {raw}");
    // and back out, one platform, to a directory (every blob is read again)
    skopeo(vec![s("copy"), s("--src-tls-verify=false"), format!("--src-creds={creds}"), dest, s("dir:/tmp/out")]).await;
    let info = owner.ok_get(&format!("/api/v1/projects/{project}/registry")).await;
    assert_eq!(info["repositories"][0]["tags_count"], 1);
}
