#!/usr/bin/env python3
"""Whether a failed merge queue run of the `ci` workflow failed for GitHub's infrastructure only,
so its pull request can go back into the queue (.github/workflows/requeue.yml, #2101).

usage:
  requeue.py <run id>                          fetch the run and print the decision
  requeue.py fetch <run id> [<attempt>]        print the run as JSON (the fixtures' format)
  requeue.py decide <run.json> [<GITHUB_OUTPUT>]  print the decision for a saved run

The decision is `requeue` only when every job that failed or was cancelled, other than the jobs
that only summarize others (`ci`, `golden (<os>) / goldens`), carries one of the annotations
GitHub writes when it, not the job, failed (INFRASTRUCTURE below) and ran no step that failed.
Anything else (a failed step, the fail-fast annotation, a time limit, a cancellation by a person
or by the fail-fast action) keeps the pull request out of the queue: a real failure is never
retried. With a GITHUB_OUTPUT file, `decide` appends `requeue=true|false`, `pr=<number>` and, to
re-queue, `why=<what happened, in words>`.

`fetch` needs `gh` (logged in, or GH_TOKEN). Run it with `python3 -I`.
"""

import json
import os
import re
import subprocess
import sys

REPO = os.environ.get("GITHUB_REPOSITORY", "velt-lang/velt")

# GitHub's annotations for a job it failed itself, each seen on merge queue runs:
#  - no runner picked the job up (macOS: 41473009735, 41485022940, 41664446634, 41673137170);
#  - the service failed while the job ran (every job of 41332184046, on all three OSes).
# Each with what the pull request's comment says happened.
INFRASTRUCTURE = {
    "The job was not acquired by Runner of type hosted even after multiple attempts": (
        "a runner was never acquired"
    ),
    "GitHub Actions has encountered an internal error when running your job.": (
        "GitHub Actions had an internal error"
    ),
}

# Jobs that only report on others: they fail because another job did.
SUMMARY = re.compile(r"^(ci|golden \(.*\) / goldens)$")

FAILED = {"failure", "cancelled", "timed_out", "startup_failure"}

QUEUE_BRANCH = re.compile(r"^gh-readonly-queue/[^/]+/pr-(\d+)-[0-9a-f]{40}$")


def gh(path):
    out = subprocess.run(
        ["gh", "api", "--paginate", "--slurp", path], capture_output=True, text=True
    )
    if out.returncode != 0:
        raise SystemExit(f"gh api {path}: {out.stderr.strip()}")
    return json.loads(out.stdout)


def fetch(run_id, attempt=None):
    """The run, its jobs and the failure annotations of the jobs that failed or were cancelled,
    with only the fields `decide` reads."""
    if not attempt:
        [run] = gh(f"repos/{REPO}/actions/runs/{run_id}")
        attempt = run["run_attempt"]
    [run] = gh(f"repos/{REPO}/actions/runs/{run_id}/attempts/{attempt}")
    jobs = []
    for page in gh(f"repos/{REPO}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100"):
        for job in page["jobs"]:
            entry = {
                "id": job["id"],
                "name": job["name"],
                "conclusion": job["conclusion"],
                "runner_name": job.get("runner_name") or "",
                "failed_steps": [
                    s["name"] for s in job.get("steps") or [] if s.get("conclusion") == "failure"
                ],
            }
            if job["conclusion"] in FAILED:
                entry["annotations"] = [
                    {"level": a["annotation_level"], "message": a["message"]}
                    for page in gh(f"repos/{REPO}/check-runs/{job['id']}/annotations")
                    for a in page
                    if a["annotation_level"] == "failure"
                ]
            jobs.append(entry)
    return {
        "id": run["id"],
        "attempt": attempt,
        "event": run["event"],
        "conclusion": run["conclusion"],
        "head_branch": run["head_branch"],
        "jobs": jobs,
    }


def infrastructure(job):
    """The INFRASTRUCTURE annotation that explains the job, or None."""
    if job["failed_steps"]:
        return None
    messages = [a["message"] for a in job.get("annotations", [])]
    for signature in INFRASTRUCTURE:
        if signature in messages:
            # Every other failure annotation must be GitHub's own consequence of it (none
            # seen so far besides the signature itself), not a step's error.
            if all(m in INFRASTRUCTURE for m in messages):
                return signature
    return None


def decide(run):
    """(requeue, pr, reasons, why): whether the run failed for the infrastructure only, and
    what happened in words (`why`)."""
    reasons = []
    why = []
    pr = None
    m = QUEUE_BRANCH.match(run.get("head_branch") or "")
    if run["event"] != "merge_group":
        reasons.append(f"not a merge queue run (event {run['event']})")
    elif not m:
        reasons.append(f"not a merge queue branch: {run.get('head_branch')!r}")
    else:
        pr = int(m.group(1))
    if run["conclusion"] not in ("failure", "cancelled"):
        reasons.append(f"the run's conclusion is {run['conclusion']}")
    failed = [j for j in run["jobs"] if j["conclusion"] in FAILED and not SUMMARY.match(j["name"])]
    if not failed:
        reasons.append("no job failed besides the summaries")
    infra = True
    for job in failed:
        signature = infrastructure(job)
        if signature:
            print(f"  infrastructure: {job['name']}: {signature}")
            if INFRASTRUCTURE[signature] not in why:
                why.append(INFRASTRUCTURE[signature])
            continue
        infra = False
        what = (
            f"step failed: {', '.join(job['failed_steps'])}"
            if job["failed_steps"]
            else "; ".join(a["message"] for a in job.get("annotations", [])) or "no annotation"
        )
        print(f"  not infrastructure: {job['name']} ({job['conclusion']}): {what}")
    if failed and not infra:
        reasons.append("a job failed for a reason other than the infrastructure")
    return (not reasons, pr, reasons, " and ".join(why))


def report(run, output=None):
    requeue, pr, reasons, why = decide(run)
    if requeue:
        print(f"decision: requeue pull request #{pr} (run {run['id']})")
    else:
        print(f"decision: leave it (run {run['id']}): {'; '.join(reasons)}")
    if output:
        with open(output, "a", encoding="utf-8", newline="\n") as f:
            f.write(f"requeue={'true' if requeue else 'false'}\npr={pr or ''}\n")
            if requeue:
                f.write(f"why={why}\n")
    return requeue


def dump(run):
    """The run as JSON with a line per job (the fixtures stay short and diff well)."""
    head = {k: v for k, v in run.items() if k != "jobs"}
    lines = [f" {json.dumps(k)}: {json.dumps(v)}," for k, v in head.items()]
    jobs = ",\n".join(f"  {json.dumps(job)}" for job in run["jobs"])
    return "{\n" + "\n".join(lines) + '\n "jobs": [\n' + jobs + "\n ]\n}\n"


def main(argv):
    if len(argv) >= 2 and argv[0] == "fetch":
        sys.stdout.buffer.write(
            dump(fetch(argv[1], argv[2] if len(argv) > 2 else None)).encode("utf-8")
        )
    elif len(argv) >= 2 and argv[0] == "decide":
        with open(argv[1], encoding="utf-8") as f:
            report(json.load(f), argv[2] if len(argv) > 2 else None)
    elif len(argv) == 1 and argv[0].isdigit():
        report(fetch(argv[0]))
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
