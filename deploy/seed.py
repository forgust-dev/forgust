#!/usr/bin/env python3
"""Fill the instance of ../compose.yml with demo data to try every feature.

    docker compose up -d --build && python3 deploy/seed.py

Creates the first administrator, registers the compose runner, then users in every
role, groups, projects with history (LFS, tags, releases; a monorepo for the Smart
View), issues, merge requests in
every state (review threads, suggestions, approvals, stacks, a conflict, a train),
CI (artifacts, test reports, environments, services, child pipelines, registry
images, schedules), webhooks, tokens and keys. Every user's password is PASSWORD.
Needs python3, git and git-lfs on the host. Run it once, on an empty instance.
"""
import json, os, random, shutil, struct, subprocess, sys, time, urllib.error, urllib.request, zlib
from pathlib import Path

HOST = os.environ.get("FORGUST_HOST") or sys.exit("set FORGUST_HOST to the address compose.yml was started with")
BASE = f"http://{HOST}:8080"
PASSWORD = os.environ.get("SEED_PASSWORD", "forgust-demo-pass")
ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / "deploy" / ".seed"  # working copies and the demo SSH keys (git-ignored)
random.seed(7)
SESSIONS = {}  # browser sessions: some changes (SSH keys) refuse tokens


def log(msg):
    print(f"\033[1;36m==>\033[0m {msg}", flush=True)


class ApiError(Exception):
    pass


class Api:
    def __init__(self, token=None):
        self.token = token
        self.cookies = None
        self.csrf = None

    def call(self, method, path, body=None, expect=(200, 201, 202, 204)):
        url = path if path.startswith("http") else BASE + ("" if path.startswith("/api/") else "/api/v1") + path
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(url, method=method, data=data)
        req.add_header("content-type", "application/json")
        if self.token:
            req.add_header("private-token", self.token)
        if self.cookies:
            req.add_header("cookie", self.cookies)
            req.add_header("x-csrf-token", self.csrf)
        try:
            with urllib.request.urlopen(req, timeout=120) as r:
                raw = r.read()
                if r.status not in expect:
                    raise ApiError(f"{method} {path}: {r.status} {raw[:500]!r}")
                set_cookie = r.headers.get_all("set-cookie") or []
                if set_cookie:
                    self.cookies = "; ".join(c.split(";", 1)[0] for c in set_cookie)
                return json.loads(raw) if raw and raw[:1] in b"[{" else raw
        except urllib.error.HTTPError as e:
            raise ApiError(f"{method} {path}: {e.code} {e.read()[:800].decode(errors='replace')}") from None

    get = lambda s, p: s.call("GET", p)
    post = lambda s, p, b=None: s.call("POST", p, {} if b is None else b)
    put = lambda s, p, b=None: s.call("PUT", p, {} if b is None else b)
    delete = lambda s, p: s.call("DELETE", p)

    @classmethod
    def login(cls, login, password):
        a = cls()
        r = a.call("POST", "/auth/login", {"login": login, "password": password})
        a.csrf = r["session"]["csrf_token"]
        return a


def sh(*cmd, cwd=None, env=None, check=True, input=None):
    r = subprocess.run(cmd, cwd=cwd, env={**os.environ, **(env or {})}, text=True, capture_output=True, input=input)
    if check and r.returncode:
        sys.exit(f"{' '.join(cmd)} failed ({r.returncode}):\n{r.stdout}\n{r.stderr}")
    return r.stdout.strip()


def compose(*args, check=True):
    return sh("docker", "compose", "-f", str(ROOT / "compose.yml"), *args, check=check)


def wait_until(what, fn, timeout=600, every=3):
    t0 = time.time()
    while True:
        try:
            v = fn()
            if v:
                return v
        except (ApiError, OSError):
            pass
        if time.time() - t0 > timeout:
            sys.exit(f"timed out waiting for {what}")
        time.sleep(every)


# --------------------------------------------------------------------------- users

USERS = {
    # username: (name, role in acme, bio)
    "alice": ("Alice Martin", "owner", "Engineering lead at Acme."),
    "bob": ("Bob Nguyen", "maintainer", "Backend, CI and releases."),
    "carol": ("Carol Diaz", "developer", "Frontend and the shop's checkout."),
    "dave": ("Dave Okafor", "reporter", "QA. Files the bugs."),
    "erin": ("Erin Kowalski", "guest", "Product manager."),
    "frank": ("Frank Moreau", None, "Contractor at Partners Ltd."),
}


def email(u):
    return f"{u}@example.com"


def bootstrap():
    log(f"waiting for {BASE}")
    wait_until("the server", lambda: urllib.request.urlopen(BASE + "/health", timeout=5).status == 200, timeout=300)
    try:
        Api.login("root", PASSWORD)
        sys.exit("root already exists: this instance is seeded (docker compose down -v to start over)")
    except ApiError:
        pass
    log("creating the administrator root")
    compose("exec", "-T", "-e", f"FORGUST_ADMIN_PASSWORD={PASSWORD}", "forgust", "forgust", "admin", "create-user",
            "--username", "root", "--email", "root@example.com", "--admin")
    root_s = Api.login("root", PASSWORD)
    SESSIONS["root"] = root_s
    tok = root_s.post("/user/tokens", {"name": "seed", "scopes": ["api", "write_repository", "admin_mode"]})["token"]
    root = Api(tok)
    root.put("/admin/settings", {"signup_enabled": True, "allow_personal_projects": True, "default_branch_name": "main"})
    apis = {"root": root}
    for u, (name, _, bio) in USERS.items():
        root.post("/admin/users", {"username": u, "email": email(u), "name": name, "password": PASSWORD, "is_admin": False})
        s = Api.login(u, PASSWORD)
        SESSIONS[u] = s
        apis[u] = Api(s.post("/user/tokens", {"name": "seed", "scopes": ["api", "write_repository"]})["token"])
    ids = {u: root.get(f"/users/{u}")["id"] for u in apis}
    return apis, ids


def register_runner():
    log("registering the compose runner (docker executor)")
    # a runner is a credential: made from a browser session, not with a token
    r = SESSIONS["root"].post("/admin/runners", {"description": "compose docker runner", "run_untagged": True, "tags": ["docker"]})
    compose("exec", "-T", "runner", "gitlab-runner", "register", "--non-interactive",
            "--url", BASE, "--token", r["token"], "--name", "compose-runner",
            "--executor", "docker", "--docker-image", "alpine:3.20", "--docker-pull-policy", "if-not-present")
    compose("exec", "-T", "runner", "sed", "-i", "s/^concurrent = .*/concurrent = 4/", "/etc/gitlab-runner/config.toml")


# --------------------------------------------------------------------------- git

class Repo:
    """A local working copy; commits are authored by demo users at past dates."""

    def __init__(self, name, pusher, token, path):
        self.dir = WORK / name
        self.dir.mkdir(parents=True)
        self.url = f"http://{pusher}:{token}@{HOST}:8080/{path}.git"
        self.day = 120  # days ago of the next commit
        self.git("init", "-q", "-b", "main")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "lfs.locksverify", "false")

    def git(self, *args, env=None, check=True):
        return sh("git", *args, cwd=self.dir, env=env, check=check)

    def write(self, files):
        for p, content in files.items():
            f = self.dir / p
            if content is None:
                f.unlink()
                continue
            f.parent.mkdir(parents=True, exist_ok=True)
            if isinstance(content, bytes):
                f.write_bytes(content)
            else:
                f.write_text(content)

    def commit(self, author, msg, files=None, days_ago=None):
        if files:
            self.write(files)
        if days_ago is None:
            self.day = max(0, self.day - random.randint(1, 6))
            days_ago = self.day
        when = time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(time.time() - days_ago * 86400 - random.randint(0, 30000)))
        name = USERS.get(author, ("Administrator",))[0]
        env = {"GIT_AUTHOR_NAME": name, "GIT_AUTHOR_EMAIL": email(author), "GIT_AUTHOR_DATE": when,
               "GIT_COMMITTER_NAME": name, "GIT_COMMITTER_EMAIL": email(author), "GIT_COMMITTER_DATE": when}
        self.git("add", "-A")
        self.git("commit", "-q", "-m", msg, env=env)
        return self.git("rev-parse", "HEAD")

    def push(self, *refs, url=None, force=False):
        self.git("push", "-q", *(["-f"] if force else []), url or self.url, *refs)


def png(w, h, seed):
    """A small gradient PNG (for LFS and avatars)."""
    rnd = random.Random(seed)
    c1 = [rnd.randrange(256) for _ in range(3)]
    c2 = [rnd.randrange(256) for _ in range(3)]
    rows = b""
    for y in range(h):
        rows += b"\0" + bytes(
            int(c1[k] + (c2[k] - c1[k]) * ((x + y) / (w + h))) for x in range(w) for k in range(3))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + \
        chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b"")


# --------------------------------------------------------------------------- shop

SHOP_CI = r"""# Pipelines for merge requests, the default branch and tags.
workflow:
  name: '$CI_COMMIT_TITLE'
  rules:
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH
    - if: $CI_COMMIT_TAG
    - if: $CI_PIPELINE_SOURCE =~ /^(web|api|schedule|trigger)$/

stages: [build, test, package, deploy]

default:
  image: python:3.15-alpine
  interruptible: true

variables:
  PYTHONDONTWRITEBYTECODE: "1"

# as on GitLab, jobs without rules run in branch and tag pipelines only: these
# run in merge request pipelines too
.every-pipeline:
  rules:
    - when: on_success

build:
  extends: .every-pipeline
  stage: build
  script:
    - printf '\033[1;34m== building shop %s\033[0m\n' "$CI_COMMIT_SHORT_SHA"
    - python -m compileall -q shop
    - mkdir -p dist && tar czf "dist/shop-$CI_COMMIT_SHORT_SHA.tar.gz" shop README.md
    - echo "BUILD_ID=$CI_PIPELINE_IID-$CI_COMMIT_SHORT_SHA" > build.env
  artifacts:
    name: shop-$CI_COMMIT_REF_SLUG
    paths: [dist/]
    reports:
      dotenv: build.env
    expire_in: 1 week

unit:
  extends: .every-pipeline
  stage: test
  needs: [build]
  parallel: 2
  script:
    - python ci/run_tests.py --shard "$CI_NODE_INDEX/$CI_NODE_TOTAL" --junit "report-$CI_NODE_INDEX.xml"
  coverage: '/^Coverage: (\d+(?:\.\d+)?)%/'
  artifacts:
    when: always
    reports:
      junit: report-*.xml

lint:
  extends: .every-pipeline
  stage: test
  needs: []
  script:
    - python ci/lint.py
  allow_failure:
    exit_codes: [3]

integration:
  extends: .every-pipeline
  stage: test
  needs: [build]
  services:
    - name: redis:7-alpine
      alias: cache
  script:
    - echo "testing build $BUILD_ID against redis"
    - python ci/redis_ping.py cache 6379

container-image:
  stage: package
  needs: [build, unit]
  image:
    name: gcr.io/go-containerregistry/crane:debug
    entrypoint: [""]
  script:
    - crane auth login "$CI_REGISTRY" -u "$CI_REGISTRY_USER" -p "$CI_REGISTRY_PASSWORD"
    - mkdir -p layer/app && tar xzf dist/*.tar.gz -C layer/app && tar cf layer.tar -C layer app
    - crane append --insecure --platform linux/amd64 -b python:3.15-alpine -f layer.tar -t "$CI_REGISTRY_IMAGE:$CI_COMMIT_SHORT_SHA"
    - crane tag --insecure "$CI_REGISTRY_IMAGE:$CI_COMMIT_SHORT_SHA" "${CI_COMMIT_TAG:-latest}"
  rules:
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH || $CI_COMMIT_TAG

docs:
  stage: package
  needs: []
  trigger:
    include: ci/docs.yml
    strategy: depend
  rules:
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH

review:
  stage: deploy
  script:
    - echo "deploying !$CI_MERGE_REQUEST_IID ($CI_MERGE_REQUEST_SOURCE_BRANCH_NAME) to $CI_ENVIRONMENT_URL"
  environment:
    name: review/$CI_COMMIT_REF_SLUG
    url: http://$CI_COMMIT_REF_SLUG.shop.example.com
    on_stop: stop_review
    auto_stop_in: 1 week
  rules:
    - if: $CI_PIPELINE_SOURCE == "merge_request_event" && $CI_MERGE_REQUEST_EVENT_TYPE != "merge_train"

stop_review:
  stage: deploy
  script:
    - echo "stopping review/$CI_COMMIT_REF_SLUG"
  environment:
    name: review/$CI_COMMIT_REF_SLUG
    action: stop
  rules:
    - if: $CI_PIPELINE_SOURCE == "merge_request_event" && $CI_MERGE_REQUEST_EVENT_TYPE != "merge_train"
      when: manual
  allow_failure: true

staging:
  stage: deploy
  script:
    - echo "deploying build $BUILD_ID to staging"
  environment:
    name: staging
    url: http://staging.shop.example.com
  rules:
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH

production:
  stage: deploy
  needs: [staging]
  resource_group: production
  script:
    - echo "deploying build $BUILD_ID to production (token $DEPLOY_TOKEN)"
  environment:
    name: production
    url: https://shop.example.com
  rules:
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH
      when: manual

release:
  stage: deploy
  image: alpine:3.20
  script:
    - echo "releasing $CI_COMMIT_TAG"
  release:
    tag_name: $CI_COMMIT_TAG
    name: Shop $CI_COMMIT_TAG
    description: Released by the pipeline of $CI_COMMIT_TAG.
  rules:
    - if: $CI_COMMIT_TAG =~ /^v\d+\.\d+\.\d+-rc/
"""

SHOP_DOCS_CI = """docs-build:
  image: alpine:3.20
  script:
    - echo "building the docs of $CI_PROJECT_PATH (parent pipeline $CI_PARENT_PIPELINE_ID)"
    - mkdir -p public && cp docs/*.md public/
  artifacts:
    paths: [public/]

docs-linkcheck:
  image: alpine:3.20
  needs: [docs-build]
  script:
    - ls public
"""

RUN_TESTS = '''"""Run the unit tests (one shard of them) and write a JUnit report."""
import argparse, sys, time, unittest, xml.etree.ElementTree as ET

p = argparse.ArgumentParser()
p.add_argument("--shard", default="1/1")
p.add_argument("--junit", default="report.xml")
a = p.parse_args()
index, total = map(int, a.shard.split("/"))

sys.path.insert(0, ".")


def flat(suite):
    for t in suite:
        yield from flat(t) if isinstance(t, unittest.TestSuite) else [t]


tests = list(flat(unittest.defaultTestLoader.discover("tests", top_level_dir=".")))
tests = [t for i, t in enumerate(sorted(tests, key=str)) if i % total == index - 1]
suite = ET.Element("testsuite", name=f"shop shard {index}/{total}", tests=str(len(tests)))
failed = 0
for t in tests:
    r = unittest.TestResult()
    t0 = time.time()
    t.run(r)
    cls, name = t.id().rsplit(".", 1)
    case = ET.SubElement(suite, "testcase", classname=cls, name=name, time=f"{time.time() - t0:.3f}")
    for kind, items in (("failure", r.failures), ("error", r.errors)):
        for _, tb in items:
            failed += 1
            ET.SubElement(case, kind, message=tb.strip().splitlines()[-1]).text = tb
    for _, why in r.skipped:
        ET.SubElement(case, "skipped", message=why)
    print(("FAIL " if r.failures or r.errors else "ok   ") + t.id())
ET.ElementTree(suite).write(a.junit, encoding="utf-8", xml_declaration=True)
print(f"{len(tests)} tests, {failed} failed")
print(f"Coverage: {80 + len(tests) % 7 + index * 1.5:.1f}%")
raise SystemExit(1 if failed else 0)
'''

LINT = '''"""A toy linter: exit 3 (allowed to fail) on style warnings."""
import pathlib, sys
warnings = [f"{p}:{i}: line longer than 100" for p in pathlib.Path("shop").rglob("*.py")
            for i, l in enumerate(p.read_text().splitlines(), 1) if len(l) > 100]
print("\\n".join(warnings) or "no warnings")
sys.exit(3 if warnings else 0)
'''

REDIS_PING = '''import socket, sys
s = socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=10)
s.sendall(b"PING\\r\\n")
reply = s.recv(64)
print("redis says", reply)
sys.exit(0 if reply.startswith(b"+PONG") else 1)
'''

CART_V1 = '''"""The shopping cart."""
from dataclasses import dataclass, field


@dataclass
class Item:
    sku: str
    price_cents: int
    quantity: int = 1


@dataclass
class Cart:
    items: list[Item] = field(default_factory=list)

    def add(self, sku: str, price_cents: int, quantity: int = 1) -> None:
        for item in self.items:
            if item.sku == sku:
                item.quantity += quantity
                return
        self.items.append(Item(sku, price_cents, quantity))

    def remove(self, sku: str) -> None:
        self.items = [i for i in self.items if i.sku != sku]

    def total_cents(self) -> int:
        return sum(i.price_cents * i.quantity for i in self.items)
'''

PRICING = '''"""Prices and taxes."""

VAT_RATE = 0.20


def with_vat(cents: int, rate: float = VAT_RATE) -> int:
    """The price with VAT, rounded to the cent."""
    return round(cents * (1 + rate))


def format_price(cents: int, currency: str = "EUR") -> str:
    return f"{cents / 100:.2f} {currency}"
'''

TEST_CART = '''import unittest

from shop.cart import Cart


class CartTest(unittest.TestCase):
    def test_add_merges_same_sku(self):
        c = Cart()
        c.add("tee", 1500)
        c.add("tee", 1500, 2)
        self.assertEqual(len(c.items), 1)
        self.assertEqual(c.items[0].quantity, 3)

    def test_total(self):
        c = Cart()
        c.add("tee", 1500, 2)
        c.add("mug", 900)
        self.assertEqual(c.total_cents(), 3900)

    def test_remove(self):
        c = Cart()
        c.add("tee", 1500)
        c.remove("tee")
        self.assertEqual(c.total_cents(), 0)

    def test_empty_cart(self):
        self.assertEqual(Cart().total_cents(), 0)
'''

TEST_PRICING = '''import unittest

from shop.pricing import format_price, with_vat


class PricingTest(unittest.TestCase):
    def test_vat(self):
        self.assertEqual(with_vat(1000), 1200)

    def test_custom_rate(self):
        self.assertEqual(with_vat(1000, 0.055), 1055)

    def test_format(self):
        self.assertEqual(format_price(1999), "19.99 EUR")

    @unittest.skip("currencies other than EUR come with #4")
    def test_format_usd(self):
        self.assertEqual(format_price(1999, "USD"), "$19.99")
'''

SHOP_README = """# Shop

The Acme web shop: cart, pricing and checkout. **This is demo data** made by
`deploy/seed.py`.

![logo](assets/logo.png)

## Getting started

```sh
python -m venv .venv && . .venv/bin/activate
python ci/run_tests.py
```

| Module | What it does | Owner |
| --- | --- | --- |
| `shop/cart.py` | the cart | @carol |
| `shop/pricing.py` | prices, VAT | @bob |
| `shop/checkout.py` | payment | @alice |

## Architecture

```mermaid
graph LR
  Browser --> API[shop API]
  API --> Cart
  API --> Pricing
  Cart --> Redis[(Redis)]
```

## Roadmap

- [x] Cart
- [x] VAT
- [ ] Discount codes (see the merge requests)
- [ ] Multi-currency

> [!NOTE]
> Releases are tagged `vX.Y.Z`; `-rc` tags are released by the pipeline.
"""


COMMUNITY_FILES = {
    "LICENSE": "MIT License\n\nCopyright (c) 2026 Acme Corp\n\nPermission is hereby granted, free of charge, to any person obtaining a copy\n"
               "of this software and associated documentation files (the \"Software\"), to deal\nin the Software without restriction.\n",
    "SECURITY.md": "# Security policy\n\nReport vulnerabilities to security@example.com, not in public issues.\nWe answer within two working days.\n",
    "CONTRIBUTING.md": "# Contributing\n\n1. Open an issue first for anything large\n2. Branch from `main`, open a merge request\n3. Pipelines must pass and a maintainer approves\n",
    "CHANGELOG.md": "# Changelog\n\n## 1.1.0 (unreleased)\n\n- Discount codes\n- VAT rounding fix\n\n## 1.0.0\n\n- Cart, pricing, checkout\n",
}

TEST_CURRENCY = '''import unittest

from shop.pricing import format_price


class CurrencyTest(unittest.TestCase):
    def test_eur(self):
        self.assertEqual(format_price(1999), "19.99 EUR")

    def test_usd(self):
        self.assertEqual(format_price(1999, "USD"), "$19.99")

    def test_gbp(self):
        self.assertEqual(format_price(1999, "GBP"), "£19.99")
'''


def seed_shop(apis, ids, acme):
    alice, bob, carol, dave, erin, root = (apis[u] for u in ("alice", "bob", "carol", "dave", "erin", "root"))
    log("acme/shop: project, labels, milestones")
    p = alice.post("/projects", {"namespace_id": acme, "name": "Shop", "path": "shop", "visibility": "internal",
                                 "description": "The Acme web shop: cart, pricing, checkout. Demo of CI, merge requests and reviews."})
    pid = p["id"]
    labels = {"bug": "#d9534f", "feature": "#5cb85c", "documentation": "#428bca", "priority::high": "#e4572e",
              "priority::low": "#a8d5e2", "needs-review": "#f0ad4e", "good first issue": "#7f8c8d", "security": "#8e44ad"}
    for n, c in labels.items():
        alice.post(f"/projects/{pid}/labels", {"name": n, "color": c, "description": f"{n} issues"})
    today = time.time()
    day = lambda d: time.strftime("%Y-%m-%d", time.localtime(today + d * 86400))
    m10 = alice.post(f"/projects/{pid}/milestones", {"title": "v1.0", "description": "First release", "start_date": day(-120), "due_date": day(-30)})
    m11 = alice.post(f"/projects/{pid}/milestones", {"title": "v1.1", "description": "Discounts and search", "start_date": day(-30), "due_date": day(21)})
    m20 = alice.post(f"/projects/{pid}/milestones", {"title": "v2.0", "description": "Multi-currency", "due_date": day(90)})

    log("acme/shop: history, LFS, CODEOWNERS")
    r = Repo("shop", "alice", alice.token, "acme/shop")
    r.git("lfs", "install", "--local")
    r.git("lfs", "track", "*.png", "*.bin")
    r.commit("alice", "Initial commit", {"README.md": "# Shop\n", ".gitignore": "__pycache__/\n.venv/\ndist/\n", ".gitattributes": (r.dir / ".gitattributes").read_text(),
                                         "pyproject.toml": '[project]\nname = "shop"\nversion = "1.0.0"\ndescription = "The Acme web shop: cart, pricing, checkout"\nrequires-python = ">=3.12"\n'})
    r.commit("carol", "Add the cart", {"shop/__init__.py": "", "shop/cart.py": CART_V1, "tests/__init__.py": "", "tests/test_cart.py": TEST_CART})
    r.commit("bob", "Add pricing with VAT", {"shop/pricing.py": PRICING, "tests/test_pricing.py": TEST_PRICING})
    r.commit("bob", "Add CI: build, tests, deploys", {".gitlab-ci.yml": SHOP_CI, "ci/run_tests.py": RUN_TESTS, "ci/lint.py": LINT,
                                                      "ci/redis_ping.py": REDIS_PING, "ci/docs.yml": SHOP_DOCS_CI})
    r.commit("alice", "Write the README and add the logo", {"README.md": SHOP_README, "assets/logo.png": png(160, 160, 1),
                                                           "docs/index.md": "# Shop docs\n\nSee the README.\n"})
    r.commit("alice", "Add CODEOWNERS", {"CODEOWNERS": "* @alice\n/shop/pricing.py @bob\n/shop/cart.py @carol\n/ci/ @bob\n"})
    r.commit("alice", "Add the license and community files", COMMUNITY_FILES)
    v100 = r.commit("carol", "Cart: keep the order items were added in", {"shop/cart.py": CART_V1.replace("    def remove", "    def skus(self) -> list[str]:\n        return [i.sku for i in self.items]\n\n    def remove")})
    r.git("tag", "-a", "v1.0.0", "-m", "Shop 1.0.0", env={"GIT_COMMITTER_NAME": "Bob Nguyen", "GIT_COMMITTER_EMAIL": email("bob")})
    r.commit("bob", "Pricing: round half up", {"shop/pricing.py": PRICING.replace("return round(cents * (1 + rate))", "return int(cents * (1 + rate) + 0.5)")})
    r.commit("alice", "Add checkout", {"shop/checkout.py": '"""Checkout."""\nfrom shop.cart import Cart\nfrom shop.pricing import with_vat\n\n\ndef amount_due(cart: Cart) -> int:\n    return with_vat(cart.total_cents())\n',
                                       "assets/catalog.bin": os.urandom(256 * 1024)})
    r.commit("carol", "docs: explain the cart", {"docs/cart.md": "# Cart\n\nItems with the same SKU merge.\n"}, days_ago=8)
    r.push("main", "v1.0.0")
    git_main = r.git("rev-parse", "HEAD")

    log("acme/shop: settings, variables, protection, approvals, webhooks")
    alice.put(f"/projects/{pid}/ci/settings", {"ci_config_path": ".gitlab-ci.yml", "default_git_depth": 20, "default_timeout_secs": 3600,
                                                "instance_runners_enabled": True, "job_token_scope_enabled": True,
                                                "merge_trains_enabled": True, "only_merge_if_pipeline_succeeds": True,
                                                "auto_cancel_redundant": True})
    alice.post(f"/projects/{pid}/variables", {"key": "DEPLOY_TOKEN", "value": "s3cr3t-deploy-token-0042", "masked": True, "protected": True,
                                              "description": "Production deploys (main only)"})
    alice.post(f"/projects/{pid}/variables", {"key": "SHOP_FEATURE_FLAGS", "value": "discounts,search", "description": "Flags for review apps"})
    alice.post(f"/projects/{pid}/protected_branches", {"name": "main", "push_access_level": "maintainer", "merge_access_level": "developer",
                                                       "allow_force_push": False, "allow_deploy_keys": False, "code_owner_approval_required": False})
    alice.post(f"/projects/{pid}/protected_branches", {"name": "release/*", "push_access_level": "maintainer", "merge_access_level": "maintainer",
                                                       "allow_force_push": False, "allow_deploy_keys": False, "code_owner_approval_required": False})
    alice.post(f"/projects/{pid}/protected_tags", {"name": "v*", "create_access_level": "maintainer"})
    alice.post(f"/projects/{pid}/protected_environments", {"name": "production", "deploy_access_level": "maintainer"})
    alice.post(f"/projects/{pid}/approval_rules", {"name": "Maintainers", "approvals_required": 1, "min_access_level": "maintainer"})
    alice.post(f"/projects/{pid}/hooks", {"url": "http://hooks:8000/acme-shop", "secret": "hook-secret",
                                          "events": ["push", "merge_request", "note", "release", "pipeline", "build"]})
    alice.post(f"/projects/{pid}/pipeline_schedules", {"description": "Nightly build", "ref": "main", "cron": "0 3 * * *",
                                                       "cron_timezone": "Europe/Paris", "active": True,
                                                       "variables": [{"key": "NIGHTLY", "value": "1"}]})
    alice.post(f"/projects/{pid}/pipeline_schedules", {"description": "Weekly report (paused)", "ref": "main", "cron": "0 8 * * 1",
                                                       "cron_timezone": "UTC", "active": False})

    log("acme/shop: releases")
    bob.post(f"/projects/{pid}/releases", {"tag_name": "v1.0.0", "name": "Shop 1.0.0", "released_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(today - 30 * 86400)),
                                           "description": "## Shop 1.0.0\n\n- Cart with merged SKUs\n- Pricing with VAT\n- CI with review apps\n\nThanks @carol and @alice!",
                                           "links": [{"name": "Changelog", "url": f"{BASE}/acme/shop/-/blob/v1.0.0/README.md", "link_type": "runbook"},
                                                     {"name": "Logo", "url": f"{BASE}/acme/shop/-/raw/v1.0.0/assets/logo.png", "link_type": "image"}]})
    bob.post(f"/projects/{pid}/repository/tags", {"tag_name": "v1.1.0-rc1", "ref": "main", "message": "Release candidate 1"})

    log("acme/shop: issues")
    issues = {}
    def issue(author, title, body, labels=(), milestone=None, assignees=(), due=None):
        i = apis[author].post(f"/projects/{pid}/issues", {"title": title, "description": body})
        # guests and outsiders may open issues, not triage them: a maintainer does
        if labels or milestone or assignees or due:
            bob.put(f"/projects/{pid}/issues/{i['iid']}", {"labels": list(labels), "milestone_id": milestone,
                                                           "assignee_ids": [ids[a] for a in assignees], "due_date": due})
        issues[title] = i["iid"]
        return i["iid"]
    i1 = issue("erin", "Discount codes at checkout", "As a customer I want to enter a discount code so that I pay less.\n\n"
               "Acceptance:\n- [ ] `SPRING10` takes 10% off\n- [ ] an unknown code shows an error\n- [ ] codes stack? (@alice to decide)",
               ["feature", "priority::high"], m11["id"], ["carol"], day(14))
    i2 = issue("dave", "Total is off by one cent with VAT", "Steps:\n1. add 3 × `mug` (333 cents)\n2. look at the total\n\n"
               "Expected `11.99 EUR`, got `11.98 EUR`.\n\n```python\n>>> with_vat(999)\n1198\n```", ["bug", "priority::high"], m11["id"], ["bob"])
    i3 = issue("erin", "Search the catalog", "A search box on top of the catalog.", ["feature"], m11["id"], ["carol"])
    i4 = issue("alice", "Multi-currency prices", "Show prices in USD and GBP. Depends on #1.", ["feature", "priority::low"], m20["id"])
    i5 = issue("dave", "Document the release process", "Nobody knows how `-rc` tags work.", ["documentation", "good first issue"])
    i6 = issue("dave", "Cart forgets items after an hour", "Probably the Redis TTL.", ["bug"], m10["id"], ["carol"])
    i7 = issue("bob", "Upgrade to Python 3.13", "Done in CI already.", [], m10["id"], ["bob"])
    i8 = issue("frank", "Card numbers appear in the logs", "Seen in the staging logs (confidential details sent by email).", ["security", "bug", "priority::high"])
    notes = [(i1, "alice", "Codes don't stack for now. @carol can you take this one?"),
             (i1, "carol", "On it, see the merge request for discounts."),
             (i2, "bob", "Rounding happens before multiplying. I'll fix `with_vat`."),
             (i2, "dave", "Still there on `v1.1.0-rc1` :cry:"),
             (i3, "carol", "Do we search descriptions too, or only names?"),
             (i3, "erin", "Names only for the first version."),
             (i6, "carol", "Fixed by raising the TTL to a day.")]
    for iid, who, body in notes:
        apis[who].post(f"/projects/{pid}/issues/{iid}/notes", {"body": body})
    for iid, who, emoji in [(i1, "bob", "thumbsup"), (i1, "dave", "thumbsup"), (i1, "erin", "tada"), (i2, "alice", "eyes"), (i3, "dave", "thumbsup")]:
        apis[who].post(f"/projects/{pid}/issues/{iid}/reactions", {"name": emoji})
    for iid in (i6, i7):
        apis["bob"].put(f"/projects/{pid}/issues/{iid}", {"state_event": "close"})
    alice.put(f"/projects/{pid}/milestones/{m10['id']}", {"state_event": "close"})

    log("acme/shop: branches and merge requests")
    carol_url = f"http://carol:{carol.token}@{HOST}:8080/acme/shop.git"
    bob_url = f"http://bob:{bob.token}@{HOST}:8080/acme/shop.git"
    mrs = {}
    def mr(author, branch, title, body, **kw):
        time.sleep(4)  # let the server process the push first (else the MR gets a second pipeline)
        m = apis[author].post(f"/projects/{pid}/merge_requests", {"source_branch": branch, "target_branch": kw.pop("target", "main"),
                                                                  "title": title, "description": body, **kw})
        mrs[branch] = m["iid"]
        return m["iid"]

    # 1. merged: the VAT fix (bob), approved by alice
    r.git("checkout", "-q", "-b", "fix/vat-rounding", "main")
    r.commit("bob", "Round VAT once, after the total", {"shop/pricing.py": PRICING.replace("return round(cents * (1 + rate))", "return round(cents + cents * rate)")}, days_ago=6)
    r.commit("bob", "Test VAT on 999 cents", {"tests/test_pricing.py": TEST_PRICING.replace("    def test_custom_rate", "    def test_vat_999(self):\n        self.assertEqual(with_vat(999), 1199)\n\n    def test_custom_rate")}, days_ago=6)
    r.push("fix/vat-rounding", url=bob_url)
    m_fix = mr("bob", "fix/vat-rounding", "Fix VAT rounding", f"Closes #{i2}.\n\nRounding now happens once, on the total.", labels=["bug"],
               milestone_id=m11["id"], reviewer_ids=[ids["alice"]], remove_source_branch=True)

    # 2. open, under review: discounts (carol), threads, a suggestion, a resolved thread
    r.git("checkout", "-q", "-b", "feature/discounts", "main")
    disc = '"""Discount codes."""\n\nCODES = {\n    "SPRING10": 0.10,\n    "WELCOME5": 0.05,\n}\n\n\ndef apply(total_cents: int, code: str) -> int:\n    rate = CODES.get(code.upper())\n    if rate is None:\n        raise ValueError(f"unknown code {code}")\n    return int(total_cents * (1 - rate))\n'
    r.commit("carol", "Add discount codes", {"shop/discounts.py": disc}, days_ago=4)
    r.commit("carol", "Test discount codes", {"tests/test_discounts.py": "import unittest\n\nfrom shop.discounts import apply\n\n\nclass DiscountTest(unittest.TestCase):\n    def test_spring(self):\n        self.assertEqual(apply(1000, 'spring10'), 900)\n\n    def test_unknown(self):\n        with self.assertRaises(ValueError):\n            apply(1000, 'NOPE')\n"}, days_ago=3)
    r.push("feature/discounts", url=carol_url)
    m_disc = mr("carol", "feature/discounts", "Add discount codes", f"Closes #{i1}.\n\n## What\n\n- `shop/discounts.py` with two codes\n- tests\n\n## To check\n\n- [x] codes are case-insensitive\n- [ ] codes from the database instead of a dict\n\n/cc @alice",
                labels=["feature", "needs-review"], milestone_id=m11["id"], assignee_ids=[ids["carol"]], reviewer_ids=[ids["bob"], ids["alice"]])

    # 3. draft: search (carol)
    r.git("checkout", "-q", "-b", "feature/search", "main")
    r.commit("carol", "WIP: search by name", {"shop/search.py": '"""Catalog search."""\n\n\ndef search(names: list[str], q: str) -> list[str]:\n    q = q.lower()\n    return [n for n in names if q in n.lower()]\n'}, days_ago=2)
    r.push("feature/search", url=carol_url)
    mr("carol", "feature/search", "Search the catalog by name", f"Part of #{i3}. Not ready: no tests yet.", draft=True, labels=["feature"], milestone_id=m11["id"])

    # 4. conflict: a change to the README lines main also changes (below)
    r.git("checkout", "-q", "-b", "docs/readme-install", "main")
    r.commit("carol", "README: install with uv", {"README.md": SHOP_README.replace("python -m venv .venv && . .venv/bin/activate", "uv venv && . .venv/bin/activate")}, days_ago=3)
    r.push("docs/readme-install", url=carol_url)
    mr("carol", "docs/readme-install", "README: install with uv", "uv is faster than venv + pip.", labels=["documentation"])

    # 5. a stack: cart persistence, then cart expiry on top of it
    r.git("checkout", "-q", "-b", "feature/cart-store", "main")
    r.commit("carol", "Store carts in Redis", {"shop/store.py": '"""Cart storage."""\nimport json\n\n\nclass Store:\n    def __init__(self, redis):\n        self.redis = redis\n\n    def save(self, key, cart):\n        self.redis.set(key, json.dumps([i.__dict__ for i in cart.items]))\n'}, days_ago=2)
    r.push("feature/cart-store", url=carol_url)
    m_store = mr("carol", "feature/cart-store", "Store carts in Redis", "First of two: storage. The expiry comes next.", labels=["feature"])
    r.git("checkout", "-q", "-b", "feature/cart-expiry")
    r.commit("carol", "Expire carts after a day", {"shop/store.py": (r.dir / "shop/store.py").read_text().replace("json.dumps([i.__dict__ for i in cart.items]))", "json.dumps([i.__dict__ for i in cart.items]), ex=86400)")}, days_ago=1)
    r.push("feature/cart-expiry", url=carol_url)
    mr("carol", "feature/cart-expiry", "Expire carts after a day", f"Stacked on !{m_store}. Fixes #{i6} for good.", stack_on_iid=m_store, target="feature/cart-store", labels=["feature"])

    # 6-7. ready for the merge train: small, approved
    for n, (branch, title, files) in enumerate([
        ("chore/gitignore", "Ignore editor files", {".gitignore": "__pycache__/\n.venv/\ndist/\n.idea/\n.vscode/\n"}),
        ("docs/cart-merging", "docs: cart merging rules", {"docs/cart.md": "# Cart\n\nItems with the same SKU merge; quantities add up.\n"}),
    ]):
        r.git("checkout", "-q", "-b", branch, "main")
        r.commit("bob", title, files, days_ago=1)
        r.push(branch, url=bob_url)
        mr("bob", branch, title, "Small change, ready for the merge train.", labels=["documentation"] if "docs" in branch else [])

    # 7b. failing tests: the test summary shows the failure and its trace
    r.git("checkout", "-q", "-b", "feature/currency", "main")
    r.commit("carol", "Format prices in other currencies", {"tests/test_currency.py": TEST_CURRENCY}, days_ago=1)
    r.push("feature/currency", url=carol_url)
    mr("carol", "feature/currency", "Prices in USD and GBP", f"Part of #{i4}. The tests fail: `format_price` ignores the currency.", labels=["feature"], milestone_id=m20["id"])

    # 8. closed
    r.git("checkout", "-q", "-b", "experiment/graphql", "main")
    r.commit("bob", "Try a GraphQL API", {"shop/graphql.py": "# experiment\n"}, days_ago=10)
    r.push("experiment/graphql", url=bob_url)
    m_gql = mr("bob", "experiment/graphql", "Experiment: GraphQL API", "Not going anywhere; keeping it for reference.")
    bob.post(f"/projects/{pid}/merge_requests/{m_gql}/close")

    # main moves on, so docs/readme-install conflicts
    r.git("checkout", "-q", "main")
    r.commit("alice", "README: Python 3.13 is required", {"README.md": SHOP_README.replace("python -m venv .venv && . .venv/bin/activate", "python3.13 -m venv .venv && . .venv/bin/activate")}, days_ago=1)
    r.push("main")

    log("acme/shop: reviews (threads, suggestions, approvals)")
    def mr_note(who, iid, body, **pos):
        return apis[who].post(f"/projects/{pid}/merge_requests/{iid}/notes", {"body": body, **({"position": pos} if pos else {})})
    t1 = mr_note("bob", m_disc, "This rounds down: `int()` truncates. Use `round()` like pricing does.\n\n```suggestion\n    return round(total_cents * (1 - rate))\n```",
                 new_path="shop/discounts.py", new_line=13)
    apis["carol"].post(f"/projects/{pid}/merge_requests/{m_disc}/notes", {"body": "Good catch, applying it.", "parent_id": t1["id"]})
    t2 = mr_note("alice", m_disc, "Should codes live in the database? Fine as a dict for v1.1.", new_path="shop/discounts.py", new_line=3)
    apis["carol"].post(f"/projects/{pid}/merge_requests/{m_disc}/notes", {"body": "Agreed, follow-up in #4.", "parent_id": t2["id"]})
    alice.post(f"/projects/{pid}/merge_requests/{m_disc}/notes/{t2['id']}/resolve", {"resolved": True})
    mr_note("dave", m_disc, "Tested on the review app: `spring10` works :tada:")
    apis["bob"].post(f"/projects/{pid}/merge_requests/{m_disc}/reactions", {"name": "rocket"})
    mr_note("alice", m_fix, "LGTM, thanks!")
    alice.post(f"/projects/{pid}/merge_requests/{m_fix}/approve")
    for b in ("chore/gitignore", "docs/cart-merging"):
        alice.post(f"/projects/{pid}/merge_requests/{mrs[b]}/approve")
    bob.post(f"/projects/{pid}/merge_requests/{m_store}/request_changes", {"body": "Needs a test with a fake Redis before it goes in."})
    return pid, mrs, m_fix


# --------------------------------------------------------------------------- other projects

API_PACKAGE = {
    "name": "acme-api", "version": "0.4.0", "private": True, "type": "module",
    "scripts": {"test": "node --test"},
    "dependencies": {"express": "4.18.2", "lodash": "4.17.20", "pino": "8.11.0", "zod": "3.20.2"},
    "devDependencies": {"eslint": "8.40.0", "prettier": "2.8.8"},
}


def seed_others(apis, ids, acme, platform, partners, shop):
    alice, bob, carol, root, frank = (apis[u] for u in ("alice", "bob", "carol", "root", "frank"))

    log("acme/platform/api: a Node service with outdated dependencies (for Renovate)")
    p = bob.post("/projects", {"namespace_id": platform, "name": "API", "path": "api", "visibility": "private",
                               "description": "Acme's public API (Node). Old dependencies: turn on Dependency updates."})
    r = Repo("api", "bob", bob.token, "acme/platform/api")
    r.commit("bob", "Initial API", {"package.json": json.dumps(API_PACKAGE, indent=2) + "\n",
                                    "src/server.js": "import express from 'express'\n\nconst app = express()\napp.get('/health', (_, res) => res.json({ ok: true }))\napp.listen(3000)\n",
                                    "test/health.test.js": "import { test } from 'node:test'\nimport assert from 'node:assert'\n\ntest('adds', () => assert.equal(1 + 1, 2))\n",
                                    "Dockerfile": "FROM node:18.20.0-alpine\nWORKDIR /app\nCOPY . .\nRUN npm ci --omit=dev\nCMD [\"node\", \"src/server.js\"]\n",
                                    ".gitlab-ci.yml": "test:\n  image: node:22-alpine\n  script:\n    - node --test\n",
                                    "README.md": "# Acme API\n\nTurn on *Settings → Dependency updates* to get Renovate merge requests.\n"})
    r.commit("alice", "Add a rate limiter", {"src/limit.js": "export const limit = (n) => (req, res, next) => next()\n"})
    r.push("main")
    bob.post(f"/projects/{p['id']}/variables", {"key": "NPM_TOKEN", "value": "npm_demo_token_123456", "masked": True, "description": "Not passed to Renovate"})

    log("acme/platform/infra: Terraform, a deploy key, a deploy token, an access token")
    p2 = alice.post("/projects", {"namespace_id": platform, "name": "Infra", "path": "infra", "visibility": "private",
                                  "description": "Terraform for Acme's cloud."})
    r = Repo("infra", "alice", alice.token, "acme/platform/infra")
    r.commit("alice", "Terraform skeleton", {"main.tf": 'terraform {\n  required_version = ">= 1.6"\n}\n\nmodule "shop" {\n  source = "./modules/app"\n  name   = "shop"\n}\n',
                                             "modules/app/main.tf": 'variable "name" {}\n\noutput "url" {\n  value = "https://${var.name}.example.com"\n}\n',
                                             "README.md": "# Infra\n\n`terraform plan` runs in CI.\n",
                                             ".gitlab-ci.yml": "plan:\n  image: alpine:3.20\n  script:\n    - echo 'terraform plan (demo)'\n    - grep -c module main.tf\n"})
    r.push("main")
    key = WORK / "deploy_key"
    sh("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "infra-deploy", "-f", str(key))
    SESSIONS["alice"].post(f"/projects/{p2['id']}/deploy_keys", {"title": "CD server", "key": (WORK / "deploy_key.pub").read_text().strip(), "can_push": False})
    SESSIONS["alice"].post(f"/projects/{p2['id']}/deploy_tokens", {"name": "k8s pull", "scopes": ["read_repository", "read_registry"]})
    SESSIONS["alice"].post(f"/projects/{p2['id']}/access_tokens", {"name": "terraform bot", "access_level": "developer", "scopes": ["api"]})

    log("acme/handbook: public docs")
    p3 = alice.post("/projects", {"namespace_id": acme, "name": "Handbook", "path": "handbook", "visibility": "public",
                                  "description": "How Acme works. Public."})
    r = Repo("handbook", "alice", alice.token, "acme/handbook")
    r.commit("alice", "Start the handbook", {"README.md": "# Acme handbook\n\n- [Onboarding](onboarding.md)\n- [Code review](review.md)\n- [On call](oncall.md)\n",
                                            "onboarding.md": "# Onboarding\n\n1. Get an account\n2. Add your SSH key\n3. Clone `acme/shop`\n\n## Tools\n\n| Tool | Why |\n| --- | --- |\n| Forgust | code, CI |\n| Mailpit | demo emails |\n"})
    r.commit("bob", "Code review guide", {"review.md": "# Code review\n\n- Review within a day\n- Use suggestions for small fixes\n- Approve when you'd be happy to maintain it\n\n```diff\n- return int(x)\n+ return round(x)\n```\n"})
    r.commit("erin", "On-call rota", {"oncall.md": "# On call\n\nWeekly rotation, Monday 9:00.\n\n```mermaid\nsequenceDiagram\n  Pager->>OnCall: alert\n  OnCall->>Team: escalate after 15 min\n```\n"})
    r.push("main")

    log("alice/notes and frank/sandbox: personal projects")
    p4 = alice.post("/projects", {"name": "Notes", "path": "notes", "visibility": "private", "description": "Alice's private notes."})
    r = Repo("notes", "alice", alice.token, "alice/notes")
    r.commit("alice", "Notes", {"README.md": "# Notes\n\n- 1:1 with Bob on Thursday\n- Ask Erin about multi-currency\n"})
    r.push("main")
    p5 = frank.post("/projects", {"name": "Sandbox", "path": "sandbox", "visibility": "public", "description": "Frank tries things."})
    r = Repo("sandbox", "frank", frank.token, "frank/sandbox")
    r.commit("frank", "Hello", {"hello.sh": "#!/bin/sh\necho hello\n", ".gitlab-ci.yml": "hello:\n  image: alpine:3.20\n  script: [sh hello.sh]\n"})
    r.push("main")

    log("sharing: acme/shop with the partners group (Reporter)")
    alice.post(f"/projects/{shop}/shares", {"group_id": partners, "max_access_level": "reporter"})
    alice.post(f"/groups/{acme}/variables", {"key": "ACME_REGISTRY_MIRROR", "value": "https://mirror.example.com", "description": "Group variable"})
    return p["id"]


# --------------------------------------------------------------------------- marketplace

# A monorepo for the Smart View: a storefront and a back office on one API, the API's crates and
# their schema, a worker and a search indexer, their images and services, and what deploys them
# (a Helm chart, Kustomize, Ansible, Terraform, Pulumi); each part with its CI jobs (one failing),
# its owners, and a history of who changed it.
MARKET_DESCRIPTION = "Acme's next shop, in one repository: open its Smart View to see the parts, what each needs and who to ask."

MARKET_README = """# Marketplace

The storefront (`apps/storefront`) and the back office (`apps/backoffice`) call the API
(`services/api`), built on its crates (`crates/`). The order worker (`services/worker`) handles
what happens after an order; the indexer (`services/indexer`) keeps search in step with the
catalog.

    docker compose up

See [docs/architecture.md](docs/architecture.md).
"""

MARKET_CARGO = """[workspace]
members = ["services/api", "crates/*"]
resolver = "3"

[workspace.dependencies]
market-domain = { path = "crates/domain" }
market-store = { path = "crates/store" }
"""

MARKET_COMPOSE = """# The whole marketplace on one machine: docker compose up
services:
  storefront:
    build:
      context: .
      dockerfile: apps/storefront/Dockerfile
    ports: ["3000:80"]
    environment:
      API_URL: http://api:8080
  api:
    build:
      context: .
      dockerfile: services/api/Dockerfile
    depends_on: [db]
    environment:
      DATABASE_URL: postgres://market:market@db:5432/market
      REDIS_URL: redis://cache:6379
      AMQP_URL: amqp://market:market@queue:5672
      SMTP_URL: smtp://mail:1025
      S3_ENDPOINT: http://s3:9000
  worker:
    build: services/worker
    depends_on: [queue]
    environment:
      AMQP_URL: amqp://market:market@queue:5672
      DATABASE_URL: postgres://market:market@db:5432/market
  indexer:
    build: services/indexer
    environment:
      MEILI_URL: http://search:7700
      DATABASE_URL: postgres://market:market@db:5432/market
  db:
    image: postgres:17
    environment:
      POSTGRES_USER: market
      POSTGRES_PASSWORD: market
  cache:
    image: valkey/valkey:8
  queue:
    image: rabbitmq:4-management
  search:
    image: getmeili/meilisearch:v1.15
  mail:
    image: axllent/mailpit:v1.27
  s3:
    image: minio/minio:RELEASE.2025-04-22T22-12-26Z
    command: server /data
"""

MARKET_CI = """# Each part is tested and built when its files change (rules: changes, some
# through extends): the Smart View shows each job on its part.
stages: [test, build]

default:
  image: alpine:3.20

.rust:
  rules:
    - changes: ["services/api/**/*", "crates/**/*", "Cargo.toml"]

api-test:
  stage: test
  extends: .rust
  script:
    - echo "cargo test --workspace (demo)"

schema-check:
  stage: test
  script:
    - echo "the migrations apply to an empty database (demo)"
  rules:
    - changes: ["crates/store/migrations/**/*"]

ui-test:
  stage: test
  script:
    - echo "npm test -w packages/ui (demo)"
  rules:
    - changes: ["packages/ui/**/*"]

worker-test:
  stage: test
  script:
    - echo "go test ./... (demo)"
    - 'echo "--- FAIL: TestRetryBackoff (0.02s)"'
    - 'echo "    orders_test.go:18: backoff after 3 tries = 8s, want 4s"'
    - exit 1
  rules:
    - changes: ["services/worker/**/*"]

indexer-test:
  stage: test
  script:
    - echo "pytest (demo)"
  rules:
    - changes: ["services/indexer/**/*"]

api-image:
  stage: build
  extends: .rust
  needs: [api-test]
  script:
    - echo "docker build -f services/api/Dockerfile . (demo)"

storefront-build:
  stage: build
  needs: []
  script:
    - echo "npm run build -w apps/storefront (demo)"
  rules:
    - changes: ["apps/storefront/**/*", "packages/**/*"]

backoffice-build:
  stage: build
  needs: []
  script:
    - echo "npm run build -w apps/backoffice (demo)"
  rules:
    - changes: ["apps/backoffice/**/*"]
"""

MARKET_ASSAY = """# The Smart View's corrections: names to read, and what no file says.
version = 1

[blocks.acme-storefront]
name = "Storefront"

[blocks.acme-backoffice]
name = "Back office"

[blocks.marketplace-api]
name = "Marketplace API"
referents = ["@alice"]

[blocks.order-worker]
name = "Order worker"

[blocks.market-indexer]
name = "Search indexer"

# both apps call the API: no manifest says so
[[links]]
from = "acme-storefront"
to = "marketplace-api"

[[links]]
from = "acme-backoffice"
to = "marketplace-api"

# a guess from compose's environment, confirmed
[[links]]
from = "api"
to = "cache"
kind = "connects"
"""

MARKET_CODEOWNERS = """# who reviews which part
/apps/ @carol
/packages/ @carol
/services/api/ @bob
/crates/ @bob
/services/worker/ @frank
/services/indexer/ @alice
/docs/ @erin
"""


MARKET_HELM = {
    "deploy/helm/marketplace/Chart.yaml": """apiVersion: v2
name: marketplace
description: The API and the storefront on Kubernetes
type: application
version: 0.3.0
appVersion: "1.4.2"
dependencies:
  - name: common
    version: 0.1.x
    repository: file://../common
""",
    "deploy/helm/marketplace/values.yaml": """api:
  replicas: 3
  image:
    repository: registry.acme.dev/marketplace/api
    tag: "1.4.2"
storefront:
  replicas: 2
  image:
    repository: registry.acme.dev/marketplace/storefront
    tag: "1.4.2"
ingress:
  host: shop.acme.dev
""",
    "deploy/helm/marketplace/templates/api.yaml": """apiVersion: apps/v1
kind: Deployment
metadata:
  name: api
  labels: {{- include "common.labels" . | nindent 4 }}
spec:
  replicas: {{ .Values.api.replicas }}
  template:
    spec:
      containers:
        - name: api
          image: "{{ .Values.api.image.repository }}:{{ .Values.api.image.tag }}"
          ports: [{ containerPort: 8080 }]
""",
    "deploy/helm/marketplace/templates/storefront.yaml": """apiVersion: apps/v1
kind: Deployment
metadata:
  name: storefront
  labels: {{- include "common.labels" . | nindent 4 }}
spec:
  replicas: {{ .Values.storefront.replicas }}
  template:
    spec:
      containers:
        - name: storefront
          image: "{{ .Values.storefront.image.repository }}:{{ .Values.storefront.image.tag }}"
""",
    "deploy/helm/common/Chart.yaml": """apiVersion: v2
name: common
description: The labels and probes every chart of the marketplace shares
type: library
version: 0.1.0
""",
    "deploy/helm/common/templates/_labels.tpl": """{{- define "common.labels" -}}
app.kubernetes.io/part-of: marketplace
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}
""",
}

MARKET_KUSTOMIZE = {
    "deploy/k8s/base/kustomization.yaml": """apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - worker.yaml
images:
  - name: registry.acme.dev/marketplace/worker
    newTag: "2.1.0"
""",
    "deploy/k8s/base/worker.yaml": """apiVersion: apps/v1
kind: Deployment
metadata:
  name: order-worker
spec:
  replicas: 1
  selector:
    matchLabels: { app: order-worker }
  template:
    metadata:
      labels: { app: order-worker }
    spec:
      containers:
        - name: worker
          image: registry.acme.dev/marketplace/worker:2.1.0
          env:
            - name: AMQP_URL
              valueFrom: { secretKeyRef: { name: order-worker, key: amqp-url } }
""",
    "deploy/k8s/overlays/staging/kustomization.yaml": """apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
namePrefix: staging-
resources:
  - ../../base
""",
    "deploy/k8s/overlays/production/kustomization.yaml": """apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - ../../base
patches:
  - path: replicas.yaml
""",
    "deploy/k8s/overlays/production/replicas.yaml": """apiVersion: apps/v1
kind: Deployment
metadata:
  name: order-worker
spec:
  replicas: 4
""",
}

MARKET_ANSIBLE = {
    "deploy/ansible/ansible.cfg": "[defaults]\ninventory = inventory/production.ini\nroles_path = roles\n",
    "deploy/ansible/inventory/production.ini": "[db]\ndb1.acme.internal\n\n[search]\nsearch1.acme.internal\n",
    "deploy/ansible/site.yml": """- name: The database
  hosts: db
  become: true
  roles: [postgres]

- name: Search
  hosts: search
  become: true
  roles: [search-indexer]
""",
    "deploy/ansible/roles/base/meta/main.yml": """galaxy_info:
  description: "What every host gets: its packages, its updates, its metrics"
  min_ansible_version: "2.16"
""",
    "deploy/ansible/roles/base/tasks/main.yml": """- name: The packages every host needs
  ansible.builtin.apt:
    name: [curl, unattended-upgrades, prometheus-node-exporter]
    state: present
    update_cache: true
""",
    "deploy/ansible/roles/postgres/meta/main.yml": """galaxy_info:
  description: PostgreSQL 17 for the marketplace, with nightly backups to S3
dependencies:
  - role: base
""",
    "deploy/ansible/roles/postgres/tasks/main.yml": """- name: PostgreSQL 17
  ansible.builtin.apt:
    name: postgresql-17
    state: present

- name: Nightly backups
  ansible.builtin.cron:
    name: pg_dump to S3
    hour: "3"
    minute: "0"
    job: /usr/local/bin/backup-to-s3
""",
    "deploy/ansible/roles/postgres/defaults/main.yml": "postgres_max_connections: 200\n",
    "deploy/ansible/roles/search-indexer/meta/main.yml": """galaxy_info:
  description: The search indexer, in a container beside Meilisearch
dependencies:
  - role: base
""",
    "deploy/ansible/roles/search-indexer/tasks/main.yml": """- name: The indexer
  community.docker.docker_container:
    name: market-indexer
    image: "registry.acme.dev/marketplace/indexer:{{ indexer_version }}"
    command: market-indexer
    restart_policy: unless-stopped
""",
}

MARKET_TERRAFORM = {
    "infra/terraform/main.tf": """terraform {
  required_version = ">= 1.6"
  backend "gcs" {
    bucket = "acme-terraform-state"
    prefix = "marketplace"
  }
}

provider "google" {
  project = var.project
  region  = var.region
}

module "network" {
  source = "./modules/network"
  region = var.region
}

module "cluster" {
  source  = "./modules/cluster"
  network = module.network.id
}
""",
    "infra/terraform/variables.tf": """variable "project" {
  type = string
}

variable "region" {
  type    = string
  default = "europe-west1"
}
""",
    "infra/terraform/modules/network/main.tf": """variable "region" {
  type = string
}

resource "google_compute_network" "main" {
  name                    = "marketplace"
  auto_create_subnetworks = false
}

output "id" {
  value = google_compute_network.main.id
}
""",
    "infra/terraform/modules/cluster/main.tf": """variable "network" {
  type = string
}

resource "google_container_cluster" "main" {
  name             = "marketplace"
  network          = var.network
  enable_autopilot = true
}
""",
}

MARKET_PULUMI = {
    "infra/pulumi/Pulumi.yaml": """name: review-apps
runtime: nodejs
description: A preview of the API for each merge request, on Cloud Run
""",
    "infra/pulumi/Pulumi.production.yaml": """config:
  gcp:region: europe-west1
  review-apps:apiImage: registry.acme.dev/marketplace/api:1.4.2
  review-apps:ttlHours: "48"
""",
    "infra/pulumi/package.json": json.dumps({"name": "review-apps", "private": True, "main": "index.ts",
                                             "dependencies": {"@pulumi/gcp": "^8.20.0", "@pulumi/pulumi": "^3.150.0"}}, indent=2) + "\n",
    "infra/pulumi/index.ts": """import * as gcp from "@pulumi/gcp";
import * as pulumi from "@pulumi/pulumi";

const config = new pulumi.Config();

// one Cloud Run service per merge request: its stack
export const url = new gcp.cloudrunv2.Service(`api-${pulumi.getStack()}`, {
  location: "europe-west1",
  template: { containers: [{ image: config.require("apiImage") }] },
}).uri;
""",
}

# a job for each, on its files: the Smart View shows it on the block
MARKET_CI_DEPLOY = """
helm-lint:
  stage: test
  script:
    - echo "helm lint deploy/helm/* (demo)"
  rules:
    - changes: ["deploy/helm/**/*"]

kustomize-build:
  stage: test
  script:
    - echo "kustomize build deploy/k8s/overlays/production (demo)"
  rules:
    - changes: ["deploy/k8s/**/*"]

ansible-lint:
  stage: test
  script:
    - echo "ansible-lint deploy/ansible (demo)"
  rules:
    - changes: ["deploy/ansible/**/*"]

terraform-plan:
  stage: test
  script:
    - echo "terraform plan (demo)"
  rules:
    - changes: ["infra/terraform/**/*"]

pulumi-preview:
  stage: test
  script:
    - echo "pulumi preview --stack production (demo)"
  rules:
    - changes: ["infra/pulumi/**/*"]
"""

MARKET_README_DEPLOY = """
The API and the storefront run on Kubernetes from a Helm chart (`deploy/helm`), the worker from
Kustomize (`deploy/k8s`), on a cloud Terraform sets up (`infra/terraform`). Ansible sets up the
database and search hosts (`deploy/ansible`); Pulumi gives each merge request a preview of the
API (`infra/pulumi`).
"""


# a merge request's: the storefront reads the stock straight from the database
MARKET_DB_CLIENT = {
    "packages/db-client/package.json": json.dumps({"name": "@acme/db-client", "description": "Queries the catalog's tables straight from Postgres",
                                                   "private": True, "type": "module", "main": "src/index.ts", "dependencies": {"pg": "8.16.0"}}, indent=2) + "\n",
    "packages/db-client/src/index.ts": "import pg from \"pg\";\n\nexport const stock = async (sku: string): Promise<number> => {\n  const db = new pg.Client();\n  await db.connect();\n  const r = await db.query(\"select stock from products where sku = $1\", [sku]);\n  return r.rows[0]?.stock ?? 0;\n};\n",
}


def market_stock_mr(r, api, pid):
    """A branch whose storefront reads the database itself: its merge request shows what it
    changes to the map, a block and a link its reviewer may turn down."""
    r.git("checkout", "-q", "-b", "storefront-stock", "main")
    pkg = json.loads(r.git("show", "main:apps/storefront/package.json"))
    pkg["dependencies"]["@acme/db-client"] = "*"
    r.commit("carol", "storefront: show the stock, read from the database", {**MARKET_DB_CLIENT, "apps/storefront/package.json": json.dumps(pkg, indent=2) + "\n"},
             days_ago=0)
    r.push("storefront-stock")
    r.git("checkout", "-q", "main")
    time.sleep(4)  # let the server process the push first
    api.post(f"/projects/{pid}/merge_requests", {"source_branch": "storefront-stock", "target_branch": "main", "title": "Show the stock on the product page",
                                                "description": "Reads the stock from the catalog's table: quicker than a call to the API."})


def market_deploy(r):
    """What deploys the marketplace, each in a few hands: for the Smart View's infrastructure."""
    r.commit("bob", "deploy: a Helm chart for the API and the storefront", MARKET_HELM)
    r.commit("frank", "deploy: the order worker on Kubernetes, with Kustomize", MARKET_KUSTOMIZE)
    r.commit("alice", "infra: the network and the cluster, in Terraform", MARKET_TERRAFORM)
    r.commit("alice", "deploy: Ansible for the database and search hosts",
             {p: c for p, c in MARKET_ANSIBLE.items() if "search-indexer" not in p and p != "deploy/ansible/site.yml"}
             | {"deploy/ansible/site.yml": MARKET_ANSIBLE["deploy/ansible/site.yml"].split("\n- name: Search")[0]})
    r.commit("frank", "deploy: the search indexer beside Meilisearch, with Ansible",
             {p: c for p, c in MARKET_ANSIBLE.items() if "search-indexer" in p or p == "deploy/ansible/site.yml"})
    r.commit("carol", "infra: a preview of the API for each merge request, with Pulumi", MARKET_PULUMI)
    r.commit("bob", "deploy: two replicas more for the API",
             {"deploy/helm/marketplace/values.yaml": MARKET_HELM["deploy/helm/marketplace/values.yaml"].replace("replicas: 3", "replicas: 5", 1)})
    r.commit("bob", "CI: lint and plan what deploys, when it changes",
             {".gitlab-ci.yml": MARKET_CI + MARKET_CI_DEPLOY, "README.md": MARKET_README.replace("\nSee [docs", MARKET_README_DEPLOY + "\nSee [docs", 1),
              "CODEOWNERS": MARKET_CODEOWNERS + "/deploy/helm/ @bob\n/deploy/k8s/ @frank\n/deploy/ansible/ @alice\n/infra/ @alice\n"})


def market_package(name, description, deps=None, **extra):
    return json.dumps({"name": name, "description": description, "private": True, "type": "module", **extra,
                       **({"dependencies": deps} if deps else {})}, indent=2) + "\n"


def market_history(r):
    """The marketplace's history, over six months: each part mostly in one or two hands."""
    r.day = 170
    rust = lambda name, desc, deps="": f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2024"\ndescription = "{desc}"\n{deps}'
    r.commit("alice", "Start the marketplace: a Cargo and an npm workspace", {
        "README.md": MARKET_README, "Cargo.toml": MARKET_CARGO, ".gitignore": "target/\nnode_modules/\ndist/\n__pycache__/\n",
        "package.json": json.dumps({"name": "marketplace", "private": True, "workspaces": ["apps/*", "packages/*"]}, indent=2) + "\n"})
    r.commit("bob", "domain: products and prices", {
        "crates/domain/Cargo.toml": rust("market-domain", "Products, carts and orders: the marketplace's types and rules"),
        "crates/domain/src/lib.rs": "//! Products, carts and orders.\npub mod money;\n\npub struct Product {\n    pub sku: String,\n    pub name: String,\n    pub price: money::Cents,\n}\n",
        "crates/domain/src/money.rs": "pub type Cents = i64;\n\npub fn with_vat(c: Cents, rate_pct: i64) -> Cents {\n    c + c * rate_pct / 100\n}\n"})
    r.commit("bob", "store: the catalog's schema and queries", {
        "crates/store/Cargo.toml": rust("market-store", "Postgres access for the API and the worker",
                                        '\n[dependencies]\nmarket-domain.workspace = true\nsqlx = { version = "0.8", features = ["postgres", "runtime-tokio"] }\n'),
        "crates/store/src/lib.rs": "//! Queries, over the schema in migrations/.\npub mod products;\n",
        "crates/store/src/products.rs": "use market_domain::Product;\n\npub async fn all(_db: &sqlx::PgPool) -> Vec<Product> {\n    Vec::new()\n}\n",
        "crates/store/migrations/0001_products.sql": "create table products (\n  sku text primary key,\n  name text not null,\n  price_cents bigint not null\n);\n"})
    r.commit("bob", "api: the catalog routes", {
        "services/api/Cargo.toml": rust("marketplace-api", "The HTTP API the storefront and the back office call",
                                        '\n[dependencies]\naxum = "0.8"\ntokio = { version = "1", features = ["full"] }\nmarket-domain.workspace = true\nmarket-store.workspace = true\n'),
        "services/api/src/main.rs": "mod routes;\n\n#[tokio::main]\nasync fn main() {\n    let app = routes::all();\n    let listener = tokio::net::TcpListener::bind(\"0.0.0.0:8080\").await.unwrap();\n    axum::serve(listener, app).await.unwrap();\n}\n",
        "services/api/src/routes.rs": "use axum::{Router, routing::get};\n\npub fn all() -> Router {\n    Router::new().route(\"/products\", get(|| async { \"[]\" }))\n}\n"})
    r.commit("carol", "ui: buttons and prices", {
        "packages/ui/package.json": market_package("@acme/ui", "Buttons, forms and prices, shared by both apps", main="src/index.ts", peerDependencies={"react": "*"}),
        "packages/ui/src/index.ts": "export { Button } from \"./Button\";\nexport { Price } from \"./Price\";\n",
        "packages/ui/src/Button.tsx": "export function Button(props: { children: React.ReactNode; onClick?: () => void }) {\n  return <button className=\"btn\" {...props} />;\n}\n",
        "packages/ui/src/Price.tsx": "export function Price({ cents }: { cents: number }) {\n  return <span className=\"price\">{(cents / 100).toFixed(2)} €</span>;\n}\n"})
    r.commit("carol", "api-client: typed calls to the API", {
        "packages/api-client/package.json": market_package("@acme/api-client", "The API's client, typed", main="src/index.ts"),
        "packages/api-client/src/index.ts": "export type Product = { sku: string; name: string; price_cents: number };\n\nexport const products = (base: string): Promise<Product[]> => fetch(`${base}/products`).then((r) => r.json());\n"})
    store_front = {"@acme/api-client": "*", "@acme/ui": "*", "react": "19.2.0", "react-dom": "19.2.0"}
    r.commit("carol", "storefront: the catalog page", {
        "apps/storefront/package.json": market_package("@acme/storefront", "The shop customers browse and buy in", store_front,
                                                       scripts={"dev": "vite", "build": "vite build"}, devDependencies={"vite": "7.1.0"}),
        "apps/storefront/index.html": "<!doctype html>\n<div id=\"root\"></div>\n<script type=\"module\" src=\"/src/main.tsx\"></script>\n",
        "apps/storefront/src/main.tsx": "import { createRoot } from \"react-dom/client\";\nimport { Catalog } from \"./Catalog\";\n\ncreateRoot(document.getElementById(\"root\")!).render(<Catalog />);\n",
        "apps/storefront/src/Catalog.tsx": "import { Price } from \"@acme/ui\";\n\nexport function Catalog() {\n  return <ul><li>Mug <Price cents={1200} /></li></ul>;\n}\n"})
    r.commit("alice", "compose: run the whole marketplace locally", {"compose.yml": MARKET_COMPOSE})
    r.commit("frank", "worker: consume order events", {
        "services/worker/go.mod": "module github.com/acme/marketplace/order-worker\n\ngo 1.24\n\nrequire (\n\tgithub.com/jackc/pgx/v5 v5.7.5\n\tgithub.com/rabbitmq/amqp091-go v1.10.0\n)\n",
        "services/worker/cmd/worker/main.go": "package main\n\nimport \"github.com/acme/marketplace/order-worker/internal/orders\"\n\nfunc main() {\n\torders.Consume()\n}\n",
        "services/worker/internal/orders/orders.go": "package orders\n\n// Consume handles the order events from the queue.\nfunc Consume() {}\n"})
    r.commit("bob", "store: orders", {"crates/store/migrations/0002_orders.sql": "create table orders (\n  id bigserial primary key,\n  created_at timestamptz not null default now()\n);\n",
                                      "crates/store/src/orders.rs": "pub async fn place(_db: &sqlx::PgPool) {}\n",
                                      "crates/store/src/lib.rs": "//! Queries, over the schema in migrations/.\npub mod orders;\npub mod products;\n"})
    r.commit("carol", "storefront: the cart", {"apps/storefront/src/Cart.tsx": "import { Button, Price } from \"@acme/ui\";\n\nexport function Cart({ total }: { total: number }) {\n  return <aside>Total <Price cents={total} /> <Button>Order</Button></aside>;\n}\n"})
    r.commit("bob", "api: orders", {"services/api/src/routes.rs": "use axum::{Router, routing::{get, post}};\n\npub fn all() -> Router {\n    Router::new()\n        .route(\"/products\", get(|| async { \"[]\" }))\n        .route(\"/orders\", post(|| async { \"{}\" }))\n}\n"})
    r.commit("frank", "worker: retry with a backoff", {
        "services/worker/internal/orders/retry.go": "package orders\n\nimport \"time\"\n\n// Backoff doubles from one second, after each failed try.\nfunc Backoff(tries int) time.Duration {\n\treturn time.Second << tries\n}\n",
        "services/worker/internal/orders/orders_test.go": "package orders\n\nimport (\n\t\"testing\"\n\t\"time\"\n)\n\nfunc TestRetryBackoff(t *testing.T) {\n\tif got := Backoff(2); got != 4*time.Second {\n\t\tt.Fatalf(\"backoff after 3 tries = %v, want 4s\", got)\n\t}\n}\n"})
    r.commit("alice", "indexer: keep search in step with the catalog", {
        "services/indexer/pyproject.toml": '[project]\nname = "market-indexer"\nversion = "0.1.0"\ndescription = "Keeps the search index in step with the catalog"\nrequires-python = ">=3.12"\ndependencies = ["meilisearch>=0.31", "psycopg[binary]>=3.2"]\n\n[project.scripts]\nmarket-indexer = "indexer.main:run"\n',
        "services/indexer/indexer/__init__.py": "",
        "services/indexer/indexer/main.py": "\"\"\"Pushes the catalog to the search index.\"\"\"\n\n\ndef run():\n    print(\"indexed 0 products\")\n",
        "services/indexer/tests/test_main.py": "from indexer.main import run\n\n\ndef test_runs():\n    run()\n"})
    back_office = {"@acme/api-client": "*", "@acme/ui": "*", "react": "19.2.0", "react-dom": "19.2.0"}
    r.commit("carol", "backoffice: the orders list", {
        "apps/backoffice/package.json": market_package("@acme/backoffice", "Where the team manages products and orders", back_office,
                                                       scripts={"dev": "vite", "build": "vite build"}, devDependencies={"vite": "7.1.0"}),
        "apps/backoffice/src/main.tsx": "import { createRoot } from \"react-dom/client\";\nimport { Orders } from \"./Orders\";\n\ncreateRoot(document.getElementById(\"root\")!).render(<Orders />);\n",
        "apps/backoffice/src/Orders.tsx": "export function Orders() {\n  return <table><tbody /></table>;\n}\n"})
    r.commit("bob", "api: an image", {"services/api/Dockerfile": "FROM rust:1.90 AS build\nWORKDIR /src\nCOPY Cargo.toml ./\nCOPY crates crates\nCOPY services/api services/api\nRUN cargo build --release -p marketplace-api\n\nFROM debian:trixie-slim\nCOPY --from=build /src/target/release/marketplace-api /usr/local/bin/\nCMD [\"marketplace-api\"]\n"})
    r.commit("carol", "storefront: an image", {"apps/storefront/Dockerfile": "FROM node:22-alpine AS build\nWORKDIR /src\nCOPY package.json ./\nCOPY packages packages\nCOPY apps/storefront apps/storefront\nRUN npm install && npm run build -w apps/storefront\n\nFROM nginx:1.27-alpine\nCOPY --from=build /src/apps/storefront/dist /usr/share/nginx/html\n"})
    r.commit("frank", "worker: an image", {"services/worker/Dockerfile": "FROM golang:1.24-alpine AS build\nWORKDIR /src\nCOPY . .\nRUN go build -o /out/worker ./cmd/worker\n\nFROM alpine:3.20\nCOPY --from=build /out/worker /usr/local/bin/worker\nCMD [\"worker\"]\n"})
    r.commit("alice", "indexer: an image", {"services/indexer/Dockerfile": "FROM python:3.13-slim\nWORKDIR /app\nCOPY . .\nRUN pip install --no-cache-dir .\nCMD [\"market-indexer\"]\n"})
    r.commit("bob", "CI: test and build each part when it changes", {".gitlab-ci.yml": MARKET_CI})
    r.commit("alice", "CODEOWNERS, and names for the Smart View", {"CODEOWNERS": MARKET_CODEOWNERS, "assay.toml": MARKET_ASSAY})
    r.commit("erin", "docs: how the parts fit", {"docs/architecture.md": "# Architecture\n\nThe apps call the API; the API writes orders, the worker ships them, the indexer\nfeeds search. Open the project's Smart View for the map.\n"})
    r.commit("frank", "worker: a dead-letter queue", {"services/worker/internal/orders/dead.go": "package orders\n\n// DeadLetter parks an event that failed too often.\nfunc DeadLetter() {}\n"})
    r.commit("carol", "ui: dark mode colours", {"packages/ui/src/theme.css": ":root { --price: #1c1915; }\n@media (prefers-color-scheme: dark) { :root { --price: #f5f5f6; } }\n"})
    r.commit("dave", "storefront: the cart total is right after a removal", {"apps/storefront/src/Cart.tsx": "import { Button, Price } from \"@acme/ui\";\n\nexport function Cart({ total }: { total: number }) {\n  return <aside>Total <Price cents={Math.max(0, total)} /> <Button>Order</Button></aside>;\n}\n"})
    r.commit("bob", "store: order events", {"crates/store/migrations/0003_order_events.sql": "create table order_events (\n  order_id bigint not null references orders,\n  kind text not null,\n  at timestamptz not null default now()\n);\n"})
    r.commit("frank", "worker: an email when an order ships", {"services/worker/internal/orders/mail.go": "package orders\n\n// Shipped mails the customer.\nfunc Shipped() {}\n"})
    r.commit("carol", "backoffice: edit products", {"apps/backoffice/src/Products.tsx": "export function Products() {\n  return <form />;\n}\n"})
    r.commit("alice", "api: a rate limit", {"services/api/src/limit.rs": "//! Requests per minute, per address.\npub const PER_MINUTE: u32 = 600;\n"})
    r.commit("bob", "domain: discounts", {"crates/domain/src/discount.rs": "use crate::money::Cents;\n\npub fn percent_off(c: Cents, pct: i64) -> Cents {\n    c - c * pct / 100\n}\n",
                                          "crates/domain/src/lib.rs": "//! Products, carts and orders.\npub mod discount;\npub mod money;\n\npub struct Product {\n    pub sku: String,\n    pub name: String,\n    pub price: money::Cents,\n}\n"})
    r.commit("erin", "docs: the runbook", {"docs/runbook.md": "# Runbook\n\n- Orders stuck: look at the worker's dead-letter queue.\n- Search stale: run `market-indexer`.\n"})
    market_deploy(r)
    r.commit("frank", "worker: metrics", {"services/worker/internal/orders/metrics.go": "package orders\n\n// Handled counts the events handled.\nvar Handled int\n"}, days_ago=9)
    r.commit("dave", "storefront: search the catalog", {"apps/storefront/src/Search.tsx": "import { products } from \"@acme/api-client\";\n\nexport function Search() {\n  void products;\n  return <input type=\"search\" />;\n}\n"}, days_ago=6)
    r.commit("alice", "indexer: a full reindex", {"services/indexer/indexer/main.py": "\"\"\"Pushes the catalog to the search index.\"\"\"\n\n\ndef run(full: bool = False):\n    print(\"indexed 0 products\" + (\" (full)\" if full else \"\"))\n"}, days_ago=4)
    r.commit("bob", "api: health and readiness", {"services/api/src/health.rs": "pub async fn health() -> &'static str {\n    \"ok\"\n}\n"}, days_ago=2)
    # the change the failing worker-test is about
    r.commit("frank", "worker: back off faster", {"services/worker/internal/orders/retry.go": "package orders\n\nimport \"time\"\n\n// Backoff doubles from one second, after each failed try.\nfunc Backoff(tries int) time.Duration {\n\treturn time.Second << (tries + 1)\n}\n"}, days_ago=1)


def seed_marketplace(apis, platform):
    """acme/platform/marketplace: a monorepo to open in the Smart View."""
    bob = apis["bob"]
    log("acme/platform/marketplace: a monorepo for the Smart View")
    p = bob.post("/projects", {"namespace_id": platform, "name": "Marketplace", "path": "marketplace", "visibility": "internal",
                               "description": MARKET_DESCRIPTION})
    r = Repo("marketplace", "bob", bob.token, "acme/platform/marketplace")
    market_history(r)
    r.push("main")
    # a job on the compose runner maps it with the assay image (built with the stack)
    bob.put(f"/projects/{p['id']}/smart_view", {"enabled": True})
    market_stock_mr(r, apis["carol"], p["id"])
    return p["id"]


SECURITY_JOB = """
# Demo security reports: canned GitLab-format reports, so the Security
# tab has findings without running scanners. Turn on Settings > Security
# scanning for real ones.
security-report:
  stage: test
  needs: []
  script:
    - cp ci/security/*.json .
  artifacts:
    reports:
      sast: gl-sast-report.json
      dependency_scanning: gl-dependency-scanning-report.json
  rules:
    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH
"""

SECURITY_SAST = {
    "version": "15.1.4",
    "scan": {"type": "sast", "status": "success", "scanner": {"id": "semgrep", "name": "Semgrep", "version": "1.86.0", "vendor": {"name": "GitLab"}},
             "analyzer": {"id": "semgrep", "name": "Semgrep", "version": "5.21.0", "vendor": {"name": "GitLab"}}},
    "vulnerabilities": [
        {"id": "d1", "name": "Deserialization of Untrusted Data", "severity": "High",
         "description": "Unpickling data from a request can run arbitrary code.",
         "location": {"file": "shop/checkout.py", "start_line": 6, "end_line": 7},
         "identifiers": [{"type": "semgrep_id", "name": "bandit.B301-1", "value": "bandit.B301-1"},
                         {"type": "cwe", "name": "CWE-502", "value": "502", "url": "https://cwe.mitre.org/data/definitions/502.html"}]},
        {"id": "d2", "name": "Use of a broken or risky cryptographic algorithm", "severity": "Medium",
         "description": "MD5 is not collision resistant.",
         "location": {"file": "shop/cart.py", "start_line": 3, "end_line": 3},
         "identifiers": [{"type": "semgrep_id", "name": "bandit.B303-1", "value": "bandit.B303-1"},
                         {"type": "cwe", "name": "CWE-327", "value": "327"}]},
    ],
}

SECURITY_DEPS = {
    "version": "15.0.7",
    "scan": {"type": "dependency_scanning", "status": "success", "scanner": {"id": "gemnasium", "name": "Gemnasium"}},
    "vulnerabilities": [
        {"id": "r1", "name": "Requests leaks Proxy-Authorization headers on redirects", "severity": "Medium",
         "description": "Requests before 2.31.0 sends Proxy-Authorization headers to destination servers on redirects to HTTPS.",
         "solution": "Upgrade to version 2.31.0 or above.",
         "location": {"file": "requirements.txt", "dependency": {"package": {"name": "requests"}, "version": "2.25.0"}},
         "identifiers": [{"type": "gemnasium", "name": "Gemnasium-2b7f", "value": "2b7f0000-0000-4000-8000-000000000001"},
                         {"type": "cve", "name": "CVE-2023-32681", "value": "CVE-2023-32681", "url": "https://nvd.nist.gov/vuln/detail/CVE-2023-32681"}]},
        {"id": "r2", "name": "Jinja2 sandbox breakout through attr filter", "severity": "High",
         "description": "Jinja2 before 3.1.5 lets sandboxed templates escape through the |attr filter.",
         "solution": "Upgrade to version 3.1.5 or above.",
         "location": {"file": "requirements.txt", "dependency": {"package": {"name": "jinja2"}, "version": "3.1.2"}},
         "identifiers": [{"type": "gemnasium", "name": "Gemnasium-9c1a", "value": "9c1a0000-0000-4000-8000-000000000002"},
                         {"type": "cve", "name": "CVE-2024-56326", "value": "CVE-2024-56326"}]},
    ],
}


def seed_security(apis, shop):
    """The shop's main branch uploads (canned) security reports."""
    alice = apis["alice"]
    log("security: acme/shop uploads demo SAST and dependency reports")
    ci = alice.call("GET", f"/api/v4/projects/{shop}/repository/files/.gitlab-ci.yml/raw?ref=main")
    ci = ci.decode() if isinstance(ci, bytes) else str(ci)
    if "security-report:" not in ci:
        ci = ci.rstrip("\n") + "\n" + SECURITY_JOB
    alice.post(f"/projects/{shop}/repository/commits", {
        "branch": "main", "commit_message": "CI: demo security reports",
        "actions": [
            {"action": "update", "file_path": ".gitlab-ci.yml", "content": ci},
            {"action": "create", "file_path": "requirements.txt", "content": "requests==2.25.0\njinja2==3.1.2\n"},
            {"action": "create", "file_path": "ci/security/gl-sast-report.json", "content": json.dumps(SECURITY_SAST, indent=2)},
            {"action": "create", "file_path": "ci/security/gl-dependency-scanning-report.json", "content": json.dumps(SECURITY_DEPS, indent=2)},
        ]})


def seed_admin_extras(apis, acme):
    """What the admin area shows: a blocked account, an archived project."""
    root, alice = apis["root"], apis["alice"]
    log("admin: a blocked account (mallory) and an archived project (acme/legacy)")
    m = root.post("/admin/users", {"username": "mallory", "email": email("mallory"), "name": "Mallory Smith", "password": PASSWORD, "is_admin": False})
    root.put(f"/admin/users/{m['id']}", {"state": "blocked"})
    p = alice.post("/projects", {"namespace_id": acme, "name": "Legacy", "path": "legacy", "visibility": "internal",
                                 "description": "The old shop, kept for reference (archived: read-only)."})
    r = Repo("legacy", "alice", alice.token, "acme/legacy")
    r.commit("alice", "The old shop", {"README.md": "# Legacy\n\nReplaced by acme/shop.\n", "shop.php": "<?php echo 'shop';\n"}, days_ago=400)
    r.commit("bob", "Last fix", {"shop.php": "<?php echo 'shop v2';\n"}, days_ago=200)
    r.push("main")
    alice.post(f"/projects/{p['id']}/issues", {"title": "Move to the new shop", "description": "Done: see acme/shop."})
    alice.post(f"/projects/{p['id']}/archive")


def seed_keys(apis):
    log("SSH keys: one per user, private keys in the seed's work dir")
    for u in ("alice", "bob", "carol"):
        k = WORK / f"id_{u}"
        sh("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", f"{u}@demo", "-f", str(k))
        SESSIONS[u].post("/user/keys", {"title": f"{u}'s laptop", "key": (WORK / f"id_{u}.pub").read_text().strip()})


def seed_import(apis, imported):
    log("import: gitlab-org/security-products/analyzers/command from gitlab.com (anonymous)")
    root = apis["root"]
    try:
        found = root.post("/imports/lookup", {"source": "gitlab", "url": "https://gitlab.com",
                                              "path": "gitlab-org/security-products/analyzers/command", "token": ""})
        projects = found.get("projects") or [found]
        p = projects[0]
        imp = root.post("/imports", {"source": "gitlab", "url": "https://gitlab.com", "target_namespace_id": imported, "token": "",
                                     "projects": [{"source_id": str(p["id"]), "source_path": p["full_path"], "target_path": "command"}]})
        wait_until("the import plan", lambda: root.get(f"/imports/{imp['id']}")["status"] != "planning", timeout=300)
        root.post(f"/imports/{imp['id']}/start", {})
        return imp["id"]
    except ApiError as e:
        print(f"   import skipped: {e}")
        return None


# --------------------------------------------------------------------------- main

def main():
    shutil.which("git-lfs") or sys.exit("git-lfs is needed")
    apis, ids = bootstrap()
    shutil.rmtree(WORK, ignore_errors=True)  # the instance is new: so are the keys
    WORK.mkdir(parents=True)
    root, alice = apis["root"], apis["alice"]
    register_runner()

    log("groups: acme (+ acme/platform), partners, imported")
    acme = alice.post("/groups", {"name": "Acme", "path": "acme", "visibility": "public", "description": "Acme Corp, makers of the web shop."})["id"]
    platform = alice.post("/groups", {"name": "Platform", "path": "platform", "parent_id": acme, "description": "Shared services."})["id"]
    partners = apis["frank"].post("/groups", {"name": "Partners", "path": "partners", "visibility": "internal", "description": "Partners Ltd."})["id"]
    imported = root.post("/groups", {"name": "Imported", "path": "imported", "description": "Projects imported from gitlab.com."})["id"]
    for u, (_, role, _) in USERS.items():
        if role and role != "owner":
            alice.post(f"/groups/{acme}/members", {"username": u, "access_level": role})
    apis["frank"].post(f"/groups/{partners}/members", {"username": "dave", "access_level": "developer"})
    alice.post(f"/groups/{platform}/members", {"username": "frank", "access_level": "developer",
                                               "expires_at": time.strftime("%Y-%m-%dT00:00:00Z", time.gmtime(time.time() + 60 * 86400))})

    seed_keys(apis)
    shop, mrs, m_fix = seed_shop(apis, ids, acme)
    seed_others(apis, ids, acme, platform, partners, shop)
    seed_marketplace(apis, platform)
    seed_admin_extras(apis, acme)
    seed_security(apis, shop)
    imp = seed_import(apis, imported)

    log("waiting for the VAT fix's pipeline, then merging it")
    def mr_pipeline_done():
        ps = apis["bob"].get(f"/projects/{shop}/merge_requests/{m_fix}/pipelines")
        ps = ps.get("pipelines", ps) if isinstance(ps, dict) else ps
        return ps and ps[0]["status"] in ("success", "failed", "canceled") and ps[0]
    p = wait_until("the VAT fix pipeline", mr_pipeline_done, timeout=1200, every=10)
    print(f"   pipeline {p['status']}")
    try:
        apis["bob"].post(f"/projects/{shop}/merge_requests/{m_fix}/merge", {"method": "squash", "remove_source_branch": True})
        print("   merged !%d" % m_fix)
    except ApiError as e:
        print(f"   merge refused: {e}")
    alice.put(f"/projects/{shop}/push_rule", {"commit_message_negative_regex": "^(fixup|squash)!", "prevent_secrets": True,
                                             "max_file_size_mb": 50, "branch_name_regex": "", "author_email_regex": "",
                                             "commit_message_regex": "", "reject_unsigned_commits": False})
    print(f"""
\033[1;32mSeeded.\033[0m  {BASE}  (mail: http://{HOST}:8027, webhooks: docker compose logs -f hooks)

  users      root (admin), alice (Owner of acme), bob (Maintainer), carol (Developer),
             dave (Reporter), erin (Guest), frank (partners group, expiring access to acme/platform)
  password   {PASSWORD} for all of them
  SSH keys   {WORK}/id_alice (also id_bob, id_carol): GIT_SSH_COMMAND="ssh -i {WORK}/id_alice" git clone ssh://git@{HOST}:2222/acme/shop.git
  import     {'running, see Admin → Imports' if imp else 'skipped'}

Try: review !{mrs['feature/discounts']} on acme/shop as alice, add !{mrs['chore/gitignore']} and !{mrs['docs/cart-merging']} to the merge train,
play the production job, turn on Dependency updates in acme/platform/api,
open the Smart View of acme/platform/marketplace.
""")


if __name__ == "__main__":
    main()
