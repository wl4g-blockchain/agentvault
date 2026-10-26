import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const script = new URL("../../../.github/scripts/upsert-pr-status-comment.mjs", import.meta.url);
const runUrl = "https://github.com/example/agentvault/actions/runs/123";
const dockerArtifact = `${runUrl}/artifacts/456`;
const kubernetesArtifact = `${runUrl}/artifacts/789`;
const baseEnvironment = {
  GITHUB_REPOSITORY: "example/agentvault",
  GH_TOKEN: "test-token-not-sent",
  GITHUB_RUN_ID: "123",
  GITHUB_RUN_ATTEMPT: "2",
  AGENTVAULT_PR_NUMBER: "4",
  AGENTVAULT_COMMENT_KIND: "ci",
  AGENTVAULT_COMMENT_PHASE: "final",
  AGENTVAULT_HEAD_SHA: "head-sha",
  AGENTVAULT_RUN_URL: runUrl,
  AGENTVAULT_COMMENT_DRY_RUN: "true",
  AGENTVAULT_BUILD_RESULT: "success",
  AGENTVAULT_E2E_DOCKER_RESULT: "success",
  AGENTVAULT_E2E_DOCKER_SUMMARY_BASE64: "",
  AGENTVAULT_E2E_DOCKER_EVIDENCE_URL: dockerArtifact,
  AGENTVAULT_E2E_KUBERNETES_RESULT: "success",
  AGENTVAULT_E2E_KUBERNETES_SUMMARY_BASE64: "",
  AGENTVAULT_E2E_KUBERNETES_EVIDENCE_URL: kubernetesArtifact,
  AGENTVAULT_DIRTY_TAG: "dirty-headsha",
  AGENTVAULT_DIRTY_CHART_VERSION: "0.1.0-dirty.headsha",
};

const encode = (value) => Buffer.from(value).toString("base64");
const run = (overrides = {}, bootstrap) => {
  const args = bootstrap ? ["--input-type=module", "--eval", bootstrap] : [fileURLToPath(script)];
  const result = spawnSync(process.execPath, args, {
    env: { ...process.env, ...baseEnvironment, ...overrides },
    encoding: "utf8",
    timeout: 10_000,
  });
  assert.equal(result.status, 0, result.stderr || result.error?.message);
  return result.stdout;
};

test("renders both Markdown summaries and maps artifact-relative links to their own artifact", () => {
  const markdown = [
    "# Payment report",
    "",
    "Run: `same-run-id`",
    "",
    "| Scenario | Evidence |",
    "| --- | --- |",
    "| x402 | [payment report](payment/s24_balance.md) |",
    "| audit | [payment/s24_audit.md](payment/s24_audit.md) |",
    "| receipt | [`chain evidence`](payment/receipt(1).md \"receipt\") |",
    "",
    "[official](https://example.com/spec#payment)",
  ].join("\n");
  const output = run({
    AGENTVAULT_E2E_DOCKER_SUMMARY_BASE64: encode(markdown),
    AGENTVAULT_E2E_KUBERNETES_SUMMARY_BASE64: encode("## Kubernetes report\n\n[ledger](payment/ledger.md)"),
  });
  assert.ok(output.includes("#### Docker E2E summary\n\n# Payment report"));
  assert.ok(output.includes("| --- | --- |"));
  assert.ok(output.includes("Run: `same-run-id`"));
  assert.ok(output.includes(`[payment report](${dockerArtifact}) (\` payment/s24_balance.md \`)`));
  assert.ok(output.includes(`[\` payment/s24_audit.md \`](${dockerArtifact})`));
  assert.ok(output.includes("[`chain evidence`](" + dockerArtifact + ")"));
  assert.ok(output.includes(`[ledger](${kubernetesArtifact}) (\` payment/ledger.md \`)`));
  assert.ok(output.includes("[official](https://example.com/spec#payment)"));
  assert.ok(!output.includes("](payment/"));
  assert.ok(!output.includes("```text"));
  assert.ok(output.includes("**CI completed successfully.**"));
});

test("preserves inline code and fenced logs instead of escaping their backticks", () => {
  const markdown = [
    "## Evidence",
    "`[literal](payment/file.md)` and ``code with ` ticks``",
    "```text",
    "[inside log](payment/log.md)",
    "```",
    "~~~json",
    '{"note":"[not a link](payment/raw.md)"}',
    "~~~",
    "[actual](payment/actual.md)",
  ].join("\n");
  const output = run({ AGENTVAULT_E2E_DOCKER_SUMMARY_BASE64: encode(markdown) });
  assert.ok(output.includes("`[literal](payment/file.md)` and ``code with ` ticks``"));
  assert.ok(output.includes("```text\n[inside log](payment/log.md)\n```"));
  assert.ok(output.includes('~~~json\n{"note":"[not a link](payment/raw.md)"}\n~~~'));
  assert.ok(output.includes(`[actual](${dockerArtifact})`));
  assert.ok(!output.includes("``\\`"));
});

test("renders relative images and reference links as artifact evidence without invented file URLs", () => {
  const output = run({ AGENTVAULT_E2E_DOCKER_SUMMARY_BASE64: encode([
    "![Screenshot](web/screenshot.png)",
    "[report][evidence]",
    "[evidence]: payment/summary.md \"Evidence\"",
  ].join("\n")) });
  assert.ok(output.includes(`[Screenshot](${dockerArtifact}) (\` web/screenshot.png \`)`));
  assert.ok(output.includes(`[report](${dockerArtifact}) (\` payment/summary.md \`)`));
  assert.ok(!output.includes("[evidence]:"));
  assert.ok(!output.includes(`${dockerArtifact}/payment/`));
  assert.ok(!output.includes("](web/"));
});

test("each backend resolves reference links and reference images within its own report", () => {
  const summary = (name) => [
    `## ${name} evidence`,
    "[report][evidence] and [EVIDENCE][] and [evidence]",
    "![Screenshot][screenshot]",
    "`[literal][evidence]`",
    "```text",
    "[unchanged][evidence]",
    "```",
    `[evidence]: payment/${name}.md`,
    `[screenshot]: web/${name}.png`,
  ].join("\n");
  const output = run({
    AGENTVAULT_E2E_DOCKER_SUMMARY_BASE64: encode(summary("docker")),
    AGENTVAULT_E2E_KUBERNETES_SUMMARY_BASE64: encode(summary("kubernetes")),
  });
  for (const [name, artifact] of [["docker", dockerArtifact], ["kubernetes", kubernetesArtifact]]) {
    assert.ok(output.includes(`[report](${artifact}) (\` payment/${name}.md \`)`));
    assert.ok(output.includes(`[EVIDENCE](${artifact}) (\` payment/${name}.md \`)`));
    assert.ok(output.includes(`[evidence](${artifact}) (\` payment/${name}.md \`)`));
    assert.ok(output.includes(`[Screenshot](${artifact}) (\` web/${name}.png \`)`));
  }
  assert.ok(!output.includes("![Screenshot]"));
  assert.ok(!output.includes("[evidence]:"));
  assert.ok(output.includes("`[literal][evidence]`"));
  assert.ok(output.includes("```text\n[unchanged][evidence]\n```"));
});

test("external reference links also stay local to each report", () => {
  const output = run({
    AGENTVAULT_E2E_DOCKER_SUMMARY_BASE64: encode('[docs][shared]\n\n[shared]: https://example.com/docker "Docker"'),
    AGENTVAULT_E2E_KUBERNETES_SUMMARY_BASE64: encode('[docs][shared]\n\n[shared]: https://example.com/kubernetes "Kubernetes"'),
  });
  assert.ok(output.includes('[docs](https://example.com/docker "Docker")'));
  assert.ok(output.includes('[docs](https://example.com/kubernetes "Kubernetes")'));
  assert.ok(!output.includes("[shared]:"));
});

test("missing artifact URLs use the run artifact entry and oversized summaries stay bounded", () => {
  const output = run({
    AGENTVAULT_E2E_DOCKER_EVIDENCE_URL: "",
    AGENTVAULT_E2E_DOCKER_SUMMARY_BASE64: encode("[evidence](payment/s24.md)"),
    AGENTVAULT_E2E_KUBERNETES_SUMMARY_BASE64: encode("# Large report\n" + "x".repeat(20_001)),
  });
  assert.ok(output.includes(`[evidence](${runUrl}#artifacts)`));
  assert.ok(output.includes("Summary exceeds the comment limit"));
  assert.ok(output.length < 4_000);
});

test("success requires build and both E2E backends to succeed", () => {
  for (const name of ["BUILD", "E2E_DOCKER", "E2E_KUBERNETES"]) {
    for (const result of ["failure", "timed_out", "cancelled", "skipped", ""]) {
      const output = run({ [`AGENTVAULT_${name}_RESULT`]: result });
      assert.ok(!output.includes("**CI completed successfully.**"), `${name}: ${result}`);
      if (["failure", "timed_out"].includes(result)) assert.ok(output.includes("**CI failed.**"));
      else if (result === "cancelled") assert.ok(output.includes("**CI was cancelled before completion"));
      else assert.ok(output.includes("**CI did not complete all required stages.**"));
    }
  }
});

test("failed or cancelled backends retain their separate partial evidence", () => {
  const output = run({
    AGENTVAULT_E2E_DOCKER_RESULT: "failure",
    AGENTVAULT_E2E_DOCKER_SUMMARY_BASE64: encode("### Docker failed\n\n[snapshot](payment/failure.md)"),
    AGENTVAULT_E2E_KUBERNETES_RESULT: "cancelled",
    AGENTVAULT_E2E_KUBERNETES_SUMMARY_BASE64: encode("### Kubernetes partial evidence"),
  });
  assert.ok(output.includes("### Docker E2E\n\n- Status: ❌ failure"));
  assert.ok(output.includes("### Kubernetes E2E\n\n- Status: 🚫 cancelled"));
  assert.ok(output.includes("### Docker failed"));
  assert.ok(output.includes("### Kubernetes partial evidence"));
  assert.ok(output.includes("partial reports do not establish a pass"));
  assert.ok(output.includes("**CI failed.**"));
});

test("started and build comments describe parallel Docker and Kubernetes jobs", () => {
  for (const phase of ["started", "build"]) {
    const output = run({ AGENTVAULT_COMMENT_PHASE: phase });
    assert.match(output, /Docker and Kubernetes E2E.*in parallel/);
    assert.ok(!output.includes("CI completed successfully"));
  }
});

const apiRun = (scenario = {}) => {
  const bootstrap = `
    const scenario = ${JSON.stringify(scenario)};
    const calls = [];
    globalThis.fetch = async (url, options = {}) => {
      calls.push({url, method:options.method || "GET", body:options.body});
      let value;
      if (url.endsWith("/pulls/4")) value = {head:{sha:scenario.headSha || "head-sha"}};
      else if (url.endsWith("/actions/runs/123")) value = {workflow_id:9,run_number:10,run_attempt:scenario.attempt || 2};
      else if (url.includes("/actions/workflows/")) value = {workflow_runs:scenario.runs || []};
      else if (url.endsWith("/comments?per_page=100")) value = scenario.comments || [];
      else value = {id:99};
      return new Response(JSON.stringify(value),{status:200});
    };
    process.on("exit", () => console.log("CALLS:" + JSON.stringify(calls)));
    await import(${JSON.stringify(script.href)});
  `;
  const output = run({ AGENTVAULT_COMMENT_DRY_RUN: "false" }, bootstrap);
  return JSON.parse(output.split("\n").find((line) => line.startsWith("CALLS:")).slice(6));
};

test("retains head, attempt, newer-run and existing-comment stale guards", () => {
  for (const scenario of [
    { headSha: "new-head" },
    { attempt: 3 },
    { runs: [{ id:124, head_sha:"head-sha", run_number:11, pull_requests:[{number:4}] }] },
    { comments: [{id:99,user:{login:"github-actions[bot]"},body:"<!-- agentvault-ci-status -->\n<!-- agentvault-ci-run:124:1 -->"}] },
    { comments: [{id:99,user:{login:"github-actions[bot]"},body:"<!-- agentvault-ci-status -->\n<!-- agentvault-ci-run:123:3 -->"}] },
  ]) {
    assert.ok(apiRun(scenario).every((call) => call.method === "GET"));
  }
});

test("updates the bot marker once and does not overwrite another account's comment", () => {
  const previous = {id:99,user:{login:"github-actions[bot]"},body:"<!-- agentvault-ci-status -->\n<!-- agentvault-ci-run:122:1 -->"};
  const calls = apiRun({comments:[previous]});
  const writes = calls.filter((call) => call.method !== "GET");
  assert.equal(writes.length, 1);
  assert.equal(writes[0].method, "PATCH");
  assert.ok(writes[0].url.endsWith("/issues/comments/99"));
  assert.ok(JSON.parse(writes[0].body).body.includes("<!-- agentvault-ci-run:123:2 -->"));
  const ownWrites = apiRun({comments:[{...previous,user:{login:"contributor"}}]}).filter((call) => call.method !== "GET");
  assert.equal(ownWrites.length, 1);
  assert.equal(ownWrites[0].method, "POST");
});

test("release comments keep their own marker and artifact rendering", () => {
  const output = run({
    AGENTVAULT_COMMENT_KIND:"release", AGENTVAULT_PREPARE_RESULT:"success", AGENTVAULT_SHOULD_RELEASE:"true",
    AGENTVAULT_RELEASE_RESULT:"success", AGENTVAULT_RELEASE_VERSION:"0.2.0", AGENTVAULT_RELEASE_TAG:"v0.2.0",
  });
  assert.ok(output.includes("<!-- agentvault-release-status -->"));
  assert.ok(output.includes("**Release completed successfully.**"));
  assert.ok(!output.includes("Kubernetes E2E"));
});
