# Code signing policy

Windows Stats releases are currently **unsigned**. No SignPath subscription, Foundation approval or signing certificate is claimed by this policy. Unsigned releases must continue to be described as unsigned.

## Intended signing process

If code signing is approved and configured, only Windows Stats binaries built from the reviewed source in [husodrn46/windows-stats](https://github.com/husodrn46/windows-stats) will be submitted. The source commit, CI run and artifact digest must be recorded for each request. A successful CI build alone is not permission to sign or publish a release.

The maintainer, [husodrn46](https://github.com/husodrn46), is responsible for this project's source and build scripts. Contributions from other people require maintainer review. Before enabling signing, repository and signing-service accounts must use multi-factor authentication, and author, reviewer and release approver roles must be configured. Each signing request requires explicit approval by a designated project approver; no workflow currently submits signing requests automatically.

Sign only our own main executable and optional sensor-helper executable. Upstream DLLs must retain their original provenance and licences; they are not submitted as binaries maintained by this project. The policy also preserves the MIT attribution to mgvs/winstats, from which Windows Stats was developed. Eligibility of this derivative project must be confirmed with the signing provider rather than assumed.

Before any signed release is published, verify the product name, version, signature, timestamp, license notices and installed/uninstalled behavior. Do not replace an existing release asset silently. Identify the signed build and its checksum separately from unsigned builds.

## Privacy policy

Windows Stats does not send telemetry or perform automatic update checks. It reads local system counters and renders them locally. Settings and the diagnostic log are stored in the current user's Windows profile. Panels and logs may include system or process details; users should review and redact them before voluntarily sharing an issue report.

Optional network latency measurement is disabled by default. When configured, it sends ICMP requests to the selected destination only while the network panel is open. Advanced sensors use an optional local LibreHardwareMonitor helper and may require a separately installed PawnIO driver and an elevated helper. The application does not install a driver automatically. These upstream components retain their own licences, listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

GitHub hosts the source, releases and issue reports; its own privacy policy applies when users visit GitHub or voluntarily submit information. No signing service currently receives artifacts or data from the application.
