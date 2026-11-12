# Browser tests

Playwright tests against a running Forgust, on a desktop and a 390 px viewport.
Every test fails on an uncaught page error or console error.

```sh
# a server with an admin account (the defaults below), then:
FORGUST_E2E_URL=http://127.0.0.1:18491 npm run e2e
```

| Variable | Default |
| --- | --- |
| `FORGUST_E2E_URL` | `http://127.0.0.1:18491` |
| `FORGUST_E2E_ADMIN` / `FORGUST_E2E_PASSWORD` | `root` / `rootpassword` |

`global-setup.ts` signs in, then creates the fixtures it needs (group `e2e`, projects
`shop` and `blog`, branches, a merge request with a line comment) through the API
and `git push` over HTTP. It reuses them when they exist, so point it at a scratch
database: tests change state (they close and reopen the MR).

Without a local browser, run the suite in the Playwright image:

```sh
docker run --rm --network host -u $(id -u):$(id -g) -e HOME=/tmp -v $PWD:/web -w /web \
  mcr.microsoft.com/playwright:v1.49.1-noble npx playwright test
```
