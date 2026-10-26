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
const kubernetesE2eResult = process.env.AGENTVAULT_E2E_KUBERNETES_RESULT;
const kubernetesE2eSummary = process.env.AGENTVAULT_E2E_KUBERNETES_SUMMARY_BASE64;
const kubernetesE2eEvidenceUrl = process.env.AGENTVAULT_E2E_KUBERNETES_EVIDENCE_URL;
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
    return "> Summary exceeds the comment limit; see the evidence artifact for the complete report.";
  }
  return summary;
};

// Summary links address files inside a downloaded artifact. GitHub cannot serve
// those paths beneath the artifact URL, and PR-relative links point elsewhere.
const isExternalLink = (destination) => /^(?:https?:|mailto:)/i.test(destination);
const inlineCode = (value) => {
  const runs = value.match(/`+/g) || [];
  const delimiter = "`".repeat(Math.max(0, ...runs.map((run) => run.length)) + 1);
  const content = value.replaceAll("|", "\\|");
  return `${delimiter} ${content} ${delimiter}`;
};

const closingDelimiter = (line, start, open, close) => {
  let depth = 0;
  for (let index = start; index < line.length; index += 1) {
    if (line[index] === "\\") { index += 1; continue; }
    if (line[index] === open) depth += 1;
    if (line[index] === close && --depth === 0) return index;
  }
  return -1;
};

const parseLinkDestination = (target) => {
  const destination = target.match(/^(?:<([^<>]*)>|(\S+))(?:\s+(?:"[^"]*"|'[^']*'|\([^)]*\)))?$/);
  return destination ? { path: destination[1] ?? destination[2], target } : undefined;
};
const referenceKey = (label) => label.trim().replace(/\s+/g, " ").toLowerCase();
const referenceDefinition = (line) => line.match(/^ {0,3}\[([^\]]+)\]:\s*(.+)$/);
const summaryLink = (label, { path, target }, image, artifactUrl) => {
  if (isExternalLink(path)) return `${image ? "!" : ""}[${label}](${target})`;
  return label === path ? `[${inlineCode(path)}](${artifactUrl})`
    : `[${label}](${artifactUrl}) (${inlineCode(path)})`;
};

const rewriteSummaryLine = (line, artifactUrl, references) => {
  let rendered = "";
  for (let index = 0; index < line.length;) {
    if (line[index] === "\\") {
      rendered += line.slice(index, index + 2);
      index += 2;
      continue;
    }
    if (line[index] === "`") {
      const delimiter = line.slice(index).match(/^`+/)[0];
      let end = line.indexOf(delimiter, index + delimiter.length);
      while (end !== -1 && (line[end - 1] === "`" || line[end + delimiter.length] === "`")) {
        end = line.indexOf(delimiter, end + delimiter.length);
      }
      if (end !== -1) {
        rendered += line.slice(index, end + delimiter.length);
        index = end + delimiter.length;
        continue;
      }
      rendered += delimiter;
      index += delimiter.length;
      continue;
    }
    const image = line[index] === "!" && line[index + 1] === "[";
    const start = image ? index + 1 : index;
    if (line[start] === "[") {
      const labelEnd = closingDelimiter(line, start, "[", "]");
      if (labelEnd !== -1) {
        const label = line.slice(start + 1, labelEnd);
        let targetEnd = labelEnd;
        let destination;
        if (line[labelEnd + 1] === "(") {
          targetEnd = closingDelimiter(line, labelEnd + 1, "(", ")");
          if (targetEnd !== -1) destination = parseLinkDestination(line.slice(labelEnd + 2, targetEnd).trim());
        } else {
          let key = label;
          if (line[labelEnd + 1] === "[") {
            targetEnd = closingDelimiter(line, labelEnd + 1, "[", "]");
            if (targetEnd !== -1) key = line.slice(labelEnd + 2, targetEnd) || label;
          }
          if (targetEnd !== -1) destination = references.get(referenceKey(key));
        }
        if (destination) {
          rendered += summaryLink(label, destination, image, artifactUrl);
          index = targetEnd + 1;
          continue;
        }
      }
    }
    rendered += line[index];
    index += 1;
  }
  return rendered;
};

const mapSummaryLines = (summary, transform) => {
  let fence;
  return summary.split("\n").map((line) => {
    const marker = line.match(/^ {0,3}(`{3,}|~{3,})(.*)$/);
    if (fence) {
      if (marker && marker[1][0] === fence.character && marker[1].length >= fence.length && !marker[2].trim()) {
        fence = undefined;
      }
      return line;
    }
    if (marker) {
      fence = { character: marker[1][0], length: marker[1].length };
      return line;
    }
    return transform(line);
  }).join("\n");
};

const renderSummary = (summary, artifactUrl) => {
  // Resolve references per report before joining backends. Shared definition
  // labels would otherwise make Kubernetes links use Docker's first definition.
  const references = new Map();
  mapSummaryLines(summary, (line) => {
    const reference = referenceDefinition(line);
    const destination = reference && parseLinkDestination(reference[2]);
    if (destination && !references.has(referenceKey(reference[1]))) {
      references.set(referenceKey(reference[1]), destination);
    }
    return line;
  });
  return mapSummaryLines(summary, (line) => {
    const reference = referenceDefinition(line);
    if (reference && references.has(referenceKey(reference[1]))) return "";
    return rewriteSummaryLine(line, artifactUrl, references);
  });
};

const renderE2e = (backend, result, encodedSummary, evidenceUrl) => {
  const artifactUrl = evidenceUrl || `${runUrl}#artifacts`;
  const summary = decodeSummary(encodedSummary);
  const details = ["", `### ${backend} E2E`, "", `- Status: ${resultIcon(result)} ${displayResult(result)}`];
  if (evidenceUrl) details.push(`- Evidence: [reports artifact](${evidenceUrl})`);
  if (result === "skipped") details.push("- This backend did not run; no payment result is established.");
  else if (result === "cancelled") details.push("- Cancelled before completion; partial reports do not establish a pass.");
  if (summary) {
    details.push("", `#### ${backend} E2E summary`, "", renderSummary(summary, artifactUrl));
  } else if (result !== "skipped") {
    details.push(`- No summary was produced; inspect the [Actions log](${runUrl}).`);
  }
  return details;
};

const renderCi = () => {
  const artifactUrl = `${runUrl}#artifacts`;
  const details = [
    marker,
    ...(runMarker ? [runMarker] : []),
    "## AgentVault PR CI",
    "",
    `- Run: [Actions log](${runUrl})`,
    `- Commit: \`${headSha}\``,
  ];

  if (phase === "started") {
    details.push("- Status: ⏳ CI started: build dirty images and validate all SDKs, then run the real Rust/Go Docker and Kubernetes E2E matrices in parallel.");
    return details.join("\n");
  }

  details.push(`- Build / UT / IT: ${resultIcon(buildResult)} ${displayResult(buildResult)}`);
  if (buildResult === "success" && dirtyTag) {
    details.push("", "### PR artifacts", "", `- Runtime: \`${runtimeImage}:${dirtyTag}\``, `- Web: \`${webImage}:${dirtyTag}\``);
    if (dirtyChartVersion) details.push(`- Helm chart: [\`agentvault-${dirtyChartVersion}.tgz\`](${artifactUrl})`);
  }

  if (phase === "build") {
    if (buildResult === "success") {
      details.push("", "- Status: ⏳ Dirty images and the chart package are ready; Docker and Kubernetes E2E are starting or running in parallel.");
    } else if (buildResult === "cancelled") {
      details.push("", "- Status: 🚫 The build/test job was cancelled; Docker and Kubernetes E2E did not run.");
    } else if (buildResult === "skipped") {
      details.push("", "- Status: ⏭️ The build/test job was skipped; Docker and Kubernetes E2E did not run.");
    } else if (failed(buildResult)) {
      details.push("", "- Status: ❌ The build/test job failed; Docker and Kubernetes E2E did not run.");
    } else {
      details.push("", "- Status: ⏳ Build and validation are pending; both E2E backends depend on their success.");
    }
    return details.join("\n");
  }

  details.push(...renderE2e("Docker", dockerE2eResult, dockerE2eSummary, dockerE2eEvidenceUrl));
  details.push(...renderE2e("Kubernetes", kubernetesE2eResult, kubernetesE2eSummary, kubernetesE2eEvidenceUrl));
  const results = [buildResult, dockerE2eResult, kubernetesE2eResult];
  if (results.every((result) => result === "success")) {
    details.push("", "**CI completed successfully.**");
  } else if (results.some(failed)) {
    details.push("", `**CI failed.** Review the [Actions log](${runUrl}) and each backend's evidence.`);
  } else if (results.includes("cancelled")) {
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
