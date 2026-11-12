#!/usr/bin/env python3
"""Run the conformance corpus through a real GitLab CE (CI lint, dry run) and
compare with `expected.json`, the same way `forgust-ci-config/tests/conformance.rs`
checks Forgust. Used by the nightly workflow; stdlib only.

    GITLAB_URL=http://localhost:8929 GITLAB_TOKEN=glpat-... ./compare_gitlab.py
"""
import base64, json, os, sys, time, urllib.error, urllib.parse, urllib.request
from pathlib import Path

URL = os.environ["GITLAB_URL"].rstrip("/")
TOKEN = os.environ["GITLAB_TOKEN"]
CASES = Path(__file__).parent / "cases"


def api(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{URL}/api/v4{path}", data=data, method=method,
                                 headers={"PRIVATE-TOKEN": TOKEN, "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"{method} {path}: {e.code} {e.read()[:300]!r}")


def files_of(case):
    out = []
    for p in sorted(case.rglob("*")):
        rel = p.relative_to(case).as_posix()
        if p.is_file() and rel not in ("expected.json", "context.json"):
            out.append({"action": "create", "file_path": rel, "encoding": "base64",
                        "content": base64.b64encode(p.read_bytes()).decode()})
    return out


def gitlab_result(case, ctx):
    project = api("POST", "/projects", {"name": f"conf-{case.name}-{int(time.time())}", "initialize_with_readme": False})
    pid = project["id"]
    try:
        api("POST", f"/projects/{pid}/repository/commits", {"branch": "main", "commit_message": case.name, "actions": files_of(case)})
        ref = ctx.get("ref", "main")
        if ctx.get("tag"):
            api("POST", f"/projects/{pid}/repository/tags", {"tag_name": ref, "ref": "main"})
        elif ref != "main":
            api("POST", f"/projects/{pid}/repository/branches", {"branch": ref, "ref": "main"})
        q = urllib.parse.urlencode({"dry_run": "true", "ref": ref, "include_jobs": "true"})
        lint = api("GET", f"/projects/{pid}/ci/lint?{q}")
        errors = " ".join(lint.get("errors") or [])
        if not lint.get("valid"):
            if "will not run" in errors or "workflow" in errors:
                return {"skipped": True}
            return {"error": errors}
        if not lint.get("jobs"):
            return {"skipped": True}
        return {"jobs": [{"name": j["name"], "stage": j["stage"], "when": j.get("when"),
                          "allow_failure": bool(j.get("allow_failure"))} for j in lint["jobs"]]}
    finally:
        api("DELETE", f"/projects/{pid}")


def check(expected, got):
    if "error" in expected:
        return "error" in got
    if expected.get("skipped"):
        return got.get("skipped") is True
    want = {j["name"]: j for j in expected["jobs"]}
    have = {j["name"]: j for j in got.get("jobs", [])}
    if set(want) != set(have):
        return False
    return all(have[n].get(k) == w[k] for n, w in want.items() for k in ("stage", "when", "allow_failure") if k in w)


def main():
    failed, known = [], []
    for case in sorted(p for p in CASES.iterdir() if p.is_dir()):
        expected = json.loads((case / "expected.json").read_text())
        ctx = json.loads((case / "context.json").read_text()) if (case / "context.json").exists() else {}
        if expected.get("gitlab_dry_run") is False or ctx.get("source", "push") != "push":
            print(f"skip  {case.name}: not reproducible with a dry run")
            continue
        got = gitlab_result(case, ctx)
        if expected.get("forgust_only"):
            known.append(case.name)
            print(f"known {case.name}: {expected['forgust_only']} (GitLab: {json.dumps(got)[:120]})")
        elif check(expected, got):
            print(f"ok    {case.name}")
        else:
            failed.append(case.name)
            print(f"DIFF  {case.name}\n      expected {json.dumps(expected)}\n      gitlab   {json.dumps(got)}")
    print(f"\n{len(failed)} difference(s), {len(known)} known")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
