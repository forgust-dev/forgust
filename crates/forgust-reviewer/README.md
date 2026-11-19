# Forgust reviewer

The AI review job of Forgust, in an image of its own: `ghcr.io/forgust-dev/forgust-reviewer`. It is released with Forgust under the same version, and Forgust runs the reviewer image of its own version by default. When someone asks for an AI review of a merge request, Forgust runs this image as a CI job on its instance runners. The job:

1. reads the changes from the runner's clone (`git diff`);
2. asks the model endpoint the administrator set;
3. writes `review.json`.

Forgust then checks every comment in that report against the merge request before a bot of the project posts it: the line must be in the diff, the version must still be current, the count is capped, and repeats are skipped. Suggested changes are checked against the file. The job holds no credential of the forge beyond its job token, which can only clone.

The image is about 27 MB: Alpine, `git`, the system's certificate authorities, and the `forgust-review` binary, running as user 10001. It has no entrypoint, because GitLab Runner's Docker executor runs the job's script through a shell.

## Providers

| `AI_REVIEW_PROVIDER` | Talks to |
|---|---|
| `openai` | Any OpenAI-compatible `/chat/completions`: OpenAI, Ollama, vLLM, llama.cpp, LiteLLM… |

A new provider is one more implementation of `provider::Provider`.

## The contract with Forgust

The server and this image share two things: the job's variables and the report (`src/report.rs`, which the server reads through this crate without its `job` feature). Forgust's settings (Admin > AI review, or `[ai_review]` in its config file) set the variables, and Forgust reads the report.

### Variables

Set by Forgust:

| Variable | |
|---|---|
| `AI_REVIEW_PROVIDER` | Which provider. |
| `AI_REVIEW_BASE_URL` | The API's base, e.g. `http://vllm:8000/v1`. |
| `AI_REVIEW_MODEL` | The model's name at the endpoint. |
| `AI_REVIEW_API_KEY` | Masked; absent for an endpoint without a key. |
| `AI_REVIEW_CA_FILE` | A file of extra certificate authorities (PEM), when set. |
| `AI_REVIEW_MAX_COMMENTS` | The comments Forgust will post at most. The prompt asks for at most that many per request. |
| `FORGUST_REVIEW_BASE_SHA`, `FORGUST_REVIEW_HEAD_SHA` | What to review: `git diff base..head`. |
| `FORGUST_REVIEW_TARGET_SHA` | The target branch side. `.forgust/review.md` (the project's guidelines) is read from there, never from the merge request's own branch. |
| `CI_PROJECT_DIR`, `CI_MERGE_REQUEST_TITLE`, `CI_MERGE_REQUEST_DESCRIPTION` | GitLab's predefined variables. |

Options: Forgust's **Reviewer options** (`[ai_review.options]`) become `AI_REVIEW_<NAME>`, and its secret options become masked `AI_REVIEW_<NAME>`. Forgust passes them through without reading them. This version reads:

| Option | Default | |
|---|---|---|
| `MAX_DIFF_BYTES` | `200000` | Changes per model request. Fit it to the model's context. |
| `MAX_REQUESTS` | `8` | Requests per review (1–32). Files past them are listed as not reviewed, never cut. |
| `TEMPERATURE` | the endpoint's | |
| `JSON_MODE` | `true` | Asks for `response_format: json_object`. It's dropped by itself when the endpoint refuses it. |
| `REQUEST_TIMEOUT_SECS` | `600` | Per request. |

### The report: `review.json`

The job uploads it as `artifacts:reports:ai_review`, also when the review fails. The process then exits non-zero.

```json
{
  "version": 1,
  "base_sha": "…",
  "head_sha": "…",
  "model": "qwen3.8-27b",
  "summary": "Markdown",
  "comments": [
    {
      "path": "shop/paging.py",
      "new_line": 8,
      "body": "Markdown",
      "suggestion": { "from_line": 8, "to_line": 8, "original": "    start = number * size", "code": "    start = (number - 1) * size" }
    }
  ],
  "skipped": [{ "path": "Cargo.lock", "reason": "generated or a lock file" }],
  "error": "only when no review could be made"
}
```

- **Lines:** `new_line` is a line of the new file, added or kept. `old_line` is a removed line of the old file. A file is named by its new path, renamed or not.
- **Suggestions:** they replace `from_line..=to_line` of the new file, and `original` must be those lines exactly. Forgust writes the ```` ```suggestion ```` block itself, only from a suggestion that holds up; a block written in `body` is removed.
- **Compatibility:** `version` changes only on a breaking change. A new optional field keeps `1`, and Forgust ignores fields it does not know. The contract matters for images of your own: the one released with a Forgust version always matches it.

## Development

From the repository's root:

```
cargo test -p forgust-reviewer
docker build -f crates/forgust-reviewer/Dockerfile -t forgust-reviewer:local .
```

Try it in a Forgust instance by setting Admin > AI review > Reviewer image to `forgust-reviewer:local`. That works with a runner whose pull policy is `if-not-present`. The server's tests play the runner with fixture reports. One of them runs a real review on a stock GitLab Runner:

```
FORGUST_TEST_RUNNER=gitlab/gitlab-runner:v19.5.3 FORGUST_TEST_AI_REVIEW_IMAGE=forgust-reviewer:local \
  cargo test -p forgust-server --test m20_ai_review a_stock_runner
```

## License

[MIT](../../LICENSE)
