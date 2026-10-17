#!/usr/bin/env node

const repository = process.env.GITHUB_REPOSITORY;
const pullRequest = process.env.AGENTVAULT_PR_NUMBER;
const token = process.env.GH_TOKEN || process.env.GITHUB_TOKEN;
const kind = process.env.AGENTVAULT_COMMENT_KIND;
const phase = process.env.AGENTVAULT_COMMENT_PHASE;
const runUrl = process.env.AGENTVAULT_RUN_URL;
const dirtyTag = process.env.AGENTVAULT_DIRTY_TAG;
const dirtyChartVersion = process.env.AGENTVAULT_DIRTY_CHART_VERSION;
const buildResult = process.env.AGENTVAULT_BUILD_RESULT;
const dockerE2eResult = process.env.AGENTVAULT_E2E_DOCKER_RESULT;
const dockerE2eSummary = process.env.AGENTVAULT_E2E_DOCKER_SUMMARY_BASE64;
const dockerE2eEvidenceUrl = process.env.AGENTVAULT_E2E_DOCKER_EVIDENCE_URL;
const headSha = process.env.AGENTVAULT_HEAD_SHA;
const releaseResult = process.env.AGENTVAULT_RELEASE_RESULT;
const prepareResult = process.env.AGENTVAULT_PREPARE_RESULT;
const releaseVersion = process.env.AGENTVAULT_RELEASE_VERSION;
const releaseTag = process.env.AGENTVAULT_RELEASE_TAG;
const shouldRelease = process.env.AGENTVAULT_SHOULD_RELEASE;
const runId = process.env.GITHUB_RUN_ID;
const runAttempt = process.env.GITHUB_RUN_ATTEMPT || "1";
const dryRun = process.env.AGENTVAULT_COMMENT_DRY_RUN === "true";

if (!repository || !pullRequest || !token || !kind || !phase || !runUrl) {
  throw new Error("Missing AgentVault PR status comment context.");
}
if (kind === "ci" && !headSha) {
  throw new Error("Missing AgentVault PR head SHA.");
}
if (kind === "ci" && !dryRun && !runId) {
  throw new Error("Missing GitHub Actions run ID.");
}

const [owner, repo] = repository.split("/", 2);
const marker = `<!-- agentvault-${kind}-status -->`;
const runMarker = runId ? `<!-- agentvault-${kind}-run:${runId}:${runAttempt} -->` : undefined;
const repositoryApi = `https://api.github.com/repos/${owner}/${repo}`;
const apiBase = `https://api.github.com/repos/${owner}/${repo}/issues/${pullRequest}/comments`;
const runtimeImage = `ghcr.io/${owner}/agentvault`;
const webImage = `ghcr.io/${owner}/agentvault-web`;

const resultIcon = (result) => {
  if (result === "success") return "✅";
  if (result === "skipped") return "⏭️";
  if (result === "cancelled") return "🚫";
  if (result === "failure" || result === "timed_out") return "❌";
  return "⏳";
};

const displayResult = (result) => result || "pending";
const failed = (result) => result === "failure" || result === "timed_out";

const decodeSummary = (encoded) => {
  if (!encoded) return undefined;
  const summary = Buffer.from(encoded, "base64").toString("utf8").trim();
  if (!summary) return undefined;
  if (summary.length > 20_000) {
    return `${summary.slice(0, 20_000)}\n\n[Summary truncated; see the evidence artifact for the complete report.]`;
  }
  return summary;
};

const renderCi = () => {
  const artifactUrl = `${runUrl}#artifacts`;
  const summary = decodeSummary(dockerE2eSummary);
  const details = [
    marker,
    ...(runMarker ? [runMarker] : []),
    "## AgentVault PR CI",
    "",
    `- Run: [Actions log](${runUrl})`,
    `- Commit: \`${headSha}\``,
  ];

  if (phase === "started") {
    details.push("- Status: ⏳ CI started: building dirty images, validating all SDKs, then running the real five-language Docker E2E.");
    return details.join("\n");
  }

  details.push(`- Build / UT / IT: ${resultIcon(buildResult)} ${displayResult(buildResult)}`);
  if (buildResult === "success" && dirtyTag) {
    details.push(
      "",
      "### PR artifacts",
      "",
      `- Runtime: \`${runtimeImage}:${dirtyTag}\``,
      `- Web: \`${webImage}:${dirtyTag}\``,
    );
    if (dirtyChartVersion) {
      details.push(`- Helm chart: [\`agentvault-${dirtyChartVersion}.tgz\`](${artifactUrl})`);
    }
  }

  if (phase === "build") {
    if (buildResult === "success") {
      details.push("", "- Status: ⏳ Dirty images and the chart package are ready; the Docker E2E matrix is running.");
    } else if (buildResult === "cancelled") {
      details.push("", "- Status: 🚫 The build/test job was cancelled before completion; unfinished checks and E2E did not run.");
    } else if (buildResult === "skipped") {
      details.push("", "- Status: ⏭️ The build/test job was skipped; E2E did not run.");
    } else {
      details.push("", "- Status: ❌ The build/test job failed; E2E did not run.");
    }
    return details.join("\n");
  }

  details.push(`- Full MarketPulse/QuantStream Docker E2E: ${resultIcon(dockerE2eResult)} ${displayResult(dockerE2eResult)}`);
  if (dockerE2eResult === "skipped") {
    details.push("  - E2E did not start because the build stage did not succeed.");
  } else if (dockerE2eResult === "cancelled") {
    details.push("  - E2E was cancelled before completion; no test failure is being reported.");
  } else {
    if (dockerE2eEvidenceUrl) {
      details.push(`  - Evidence: [reports](${dockerE2eEvidenceUrl})`);
    }
    if (summary) {
      details.push("", "### Docker E2E summary", "", "```text", summary.replaceAll("```", "``\\`"), "```");
    } else {
      details.push(`  - No E2E report was produced; inspect the [Actions log](${runUrl}).`);
    }
  }

  const passed = buildResult === "success" && dockerE2eResult === "success";
  if (passed) {
    details.push("", "**CI completed successfully.**");
  } else if (failed(buildResult) || failed(dockerE2eResult)) {
    details.push("", `**CI failed.** Review the [Actions log](${runUrl})${dockerE2eEvidenceUrl ? " and E2E evidence" : ""}.`);
  } else if (buildResult === "cancelled" || dockerE2eResult === "cancelled") {
    details.push("", "**CI was cancelled before completion; this is not a test failure.**");
  } else {
    details.push("", `**CI did not complete all required stages.** Review the [Actions log](${runUrl}).`);
  }
  return details.join("\n");
};

const renderRelease = () => {
  const details = [
    marker,
    ...(runMarker ? [runMarker] : []),
    "## AgentVault Release",
    "",
    `- Run: [Actions log](${runUrl})`,
  ];

  if (phase === "started") {
    details.push("- Status: ⏳ Release started; evaluating the merged PR title and preparing immutable artifacts.");
    return details.join("\n");
  }
  if (prepareResult && prepareResult !== "success") {
    const message = prepareResult === "cancelled"
      ? "**Release preparation was cancelled; this is not a build failure.**"
      : prepareResult === "skipped"
        ? "**Release preparation was skipped.**"
        : `**Release preparation failed.** Review the [Actions log](${runUrl}).`;
    details.push("", message);
    return details.join("\n");
  }
  if (shouldRelease !== "true") {
    details.push("- Status: ⏭️ No release. Only merged PR titles beginning with `refactor:`, `feat:`, `fix:`, or `ci:` publish artifacts.");
    return details.join("\n");
  }

  details.push(`- Version: \`${releaseVersion}\` (${releaseTag})`);
  if (releaseResult === "success") {
    details.push(
      "",
      "### Official artifacts",
      "",
      `- Runtime: \`${runtimeImage}:${releaseVersion}\``,
      `- Web: \`${webImage}:${releaseVersion}\``,
      `- Helm chart: \`oci://ghcr.io/${owner}/charts/agentvault --version ${releaseVersion}\``,
      `- Release: [${releaseTag}](https://github.com/${repository}/releases/tag/${releaseTag})`,
      "",
      "**Release completed successfully.**",
    );
  } else if (releaseResult === "cancelled") {
    details.push("", "**Release publishing was cancelled before completion; this is not a build failure.**");
  } else if (releaseResult === "skipped") {
    details.push("", "**Release publishing was skipped; no official artifacts were published.**");
  } else {
    details.push("", `**Release failed.** Review the [Actions log](${runUrl}).`);
  }
  return details.join("\n");
};

const body = kind === "ci" ? renderCi() : renderRelease();
if (dryRun) {
  console.log(body);
  process.exit(0);
}

const headers = {
  Accept: "application/vnd.github+json",
  Authorization: `Bearer ${token}`,
  "X-GitHub-Api-Version": "2022-11-28",
  "Content-Type": "application/json",
};
const request = async (url, options = {}) => {
  const response = await fetch(url, { ...options, headers: { ...headers, ...options.headers } });
  if (!response.ok) {
    throw new Error(`GitHub API ${options.method || "GET"} ${url} failed: ${response.status} ${await response.text()}`);
  }
  return response.status === 204 ? undefined : response.json();
};

const isCurrentCiRun = async () => {
  if (kind !== "ci") return true;

  const pull = await request(`${repositoryApi}/pulls/${pullRequest}`);
  if (pull.head?.sha !== headSha) {
    console.log(`Skipping stale CI comment: PR #${pullRequest} now points to ${pull.head?.sha || "an unknown SHA"}, not ${headSha}.`);
    return false;
  }

  const currentRun = await request(`${repositoryApi}/actions/runs/${runId}`);
  const currentAttempt = Number(runAttempt);
  if (Number(currentRun.run_attempt || 1) > currentAttempt) {
    console.log(`Skipping stale CI comment: run ${runId} has a newer attempt.`);
    return false;
  }

  const workflowRuns = await request(
    `${repositoryApi}/actions/workflows/${currentRun.workflow_id}/runs?event=pull_request&head_sha=${encodeURIComponent(headSha)}&per_page=30`,
  );
  const newerRun = workflowRuns.workflow_runs?.find((candidate) => {
    const samePullRequest = candidate.pull_requests?.length
      ? candidate.pull_requests.some((pullRequestRef) => String(pullRequestRef.number) === String(pullRequest))
      : candidate.head_sha === headSha;
    return candidate.head_sha === headSha
      && samePullRequest
      && Number(candidate.run_number) > Number(currentRun.run_number);
  });
  if (newerRun) {
    console.log(`Skipping stale CI comment: newer run ${newerRun.id} already exists for PR #${pullRequest}.`);
    return false;
  }
  return true;
};

if (!(await isCurrentCiRun())) {
  process.exit(0);
}

const comments = await request(`${apiBase}?per_page=100`);
const existing = comments.find(
  (comment) => comment.user?.login === "github-actions[bot]" && comment.body?.includes(marker),
);
const existingRun = existing?.body?.match(/<!-- agentvault-ci-run:(\d+):(\d+) -->/);
if (kind === "ci" && existingRun) {
  const [, existingRunId, existingAttempt] = existingRun;
  if (Number(existingRunId) > Number(runId)
    || (existingRunId === runId && Number(existingAttempt) > Number(runAttempt))) {
    console.log(`Skipping stale CI comment: the PR comment already belongs to run ${existingRunId}, attempt ${existingAttempt}.`);
    process.exit(0);
  }
}
if (existing) {
  await request(
    `https://api.github.com/repos/${owner}/${repo}/issues/comments/${existing.id}`,
    { method: "PATCH", body: JSON.stringify({ body }) },
  );
} else {
  await request(apiBase, { method: "POST", body: JSON.stringify({ body }) });
}
