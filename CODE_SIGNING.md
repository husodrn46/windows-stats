# Code signing policy

Windows Stats releases are currently **unsigned**. No SignPath subscription, Foundation approval or signing certificate is claimed by this policy. Unsigned releases must continue to be described as unsigned.

## Intended signing process

If code signing is approved and configured, only Windows Stats binaries built from the reviewed source in [husodrn46/windows-stats](https://github.com/husodrn46/windows-stats) will be submitted. The source commit, CI run and artifact digest must be recorded for each request. A successful CI build alone is not permission to sign or publish a release.

The maintainer, [husodrn46](https://github.com/husodrn46), is responsible for this project's source and build scripts. Contributions from other people require maintainer review. Before enabling signing, repository and signing-service accounts must use multi-factor authentication, and author, reviewer and release approver roles must be configured. Each signing request requires explicit approval by a designated project approver; no workflow currently submits signing requests automatically.

## Team and roles

| Role | Members | Responsibility |
|---|---|---|
| Author (committer) | [husodrn46](https://github.com/husodrn46) | May change the source and build scripts. |
| Reviewer | [husodrn46](https://github.com/husodrn46) | Reviews every change from non-committers (pull requests from forks) before it is merged, including build scripts and CI configuration. |
| Approver | [husodrn46](https://github.com/husodrn46) | Approves each release and each signing request. |

All members use multi-factor authentication on GitHub, and will on the signing service.

Sign only our own main executable and optional sensor-helper executable. Upstream DLLs must retain their original provenance and licences; they are not submitted as binaries maintained by this project. The policy also preserves the MIT attribution to mgvs/winstats, from which Windows Stats was developed. Eligibility of this derivative project must be confirmed with the signing provider rather than assumed.

Before any signed release is published, verify the product name, version, signature, timestamp, license notices and installed/uninstalled behavior. Do not replace an existing release asset silently. Identify the signed build and its checksum separately from unsigned builds.

## Privacy policy

Windows Stats does not send telemetry. It reads local system counters and renders them locally. Update checks are optional: Settings > About checks only when clicked, and the automatic check (at most once a day) runs only if the user agreed when asked on first start or turned it on in Settings > Privacy. A check is one HTTPS request to api.github.com that carries no settings, readings or identifiers; GitHub receives the request's IP address and the Windows Stats version as user agent. Installing an update downloads the release archive from GitHub and verifies it against the size and SHA-256 digest GitHub reports. Because releases are unsigned, this verifies integrity against the published release, not the publisher's identity. Settings and the diagnostic log are stored in the current user's Windows profile. Panels and logs may include system or process details; users should review and redact them before voluntarily sharing an issue report.

Local AI session following is optional and asked once, the first time a model runs: Windows Stats then reads the names of running programs on this computer every three seconds, asks the runtime for its model name at its own loopback address (127.0.0.1), and keeps the sessions in a file in the user's settings folder. Nothing of this leaves the computer. The MCP server (`--mcp`) runs only when an AI app starts it, communicates through pipes without opening a port, and is read-only; program names are given only by its `top_processes` tool, and an AI app using a cloud model sends what it reads to that service.

Optional network latency measurement is disabled by default. When configured, it sends ICMP requests to the selected destination only while the network panel is open. Advanced sensors use an optional local LibreHardwareMonitor helper that always runs without elevation in the current unsigned, per-user build. Some PawnIO-backed readings require an elevated helper and are therefore unavailable. The application does not install a driver automatically. These upstream components retain their own licences, listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

GitHub hosts the source, releases and issue reports; its own privacy policy applies when users visit GitHub or voluntarily submit information. No signing service currently receives artifacts or data from the application.
