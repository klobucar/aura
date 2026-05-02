# Security policy

Aura is **pre-alpha**. The wire format, on-disk schemas, and cryptographic protocol can change without notice. Do not deploy it for sensitive communication. That said: it is a security/crypto project, and we take vulnerabilities in it seriously.

## Reporting a vulnerability

**Please do not file public GitHub issues for security problems.**

Email: **jon@gnupg.net**
Subject prefix: `[aura-security]`

Please include:

- A description of the issue and its impact.
- The affected component (`aura-server`, `aura-core`, `clients/macos`, `clients/desktop`, or a doc/spec).
- Steps to reproduce, ideally with a minimal proof-of-concept.
- Your name/handle for credit, or a note that you wish to remain anonymous.

You should expect an acknowledgement within **7 days**. If you don't hear back in that window, please re-send — the report may have been missed.

GitHub's private vulnerability reporting (Security tab → "Report a vulnerability") is also accepted and routes to the same person.

## Disclosure timeline

Aura follows a coordinated-disclosure model:

- We aim to ship a fix or mitigation within **90 days** of a confirmed report.
- We will credit reporters in the release notes unless asked not to.
- Embargoes can be extended by mutual agreement when a fix is genuinely complex; we will not extend indefinitely.

If a vulnerability is being actively exploited in the wild, we will prioritize a fix and a public advisory over the standard timeline.

## Scope

In scope — please report:

- Breaks in **confidentiality** of voice or text payloads (DAVE / MLS).
- Breaks in **integrity** or **authenticity** (forged frames accepted, sender-binding bypass).
- Breaks in **forward secrecy** across MLS membership changes.
- Authentication / TOFU bypasses against the server (`aura-server`).
- Memory-safety bugs in `aura-core` reachable from untrusted input (network, file).
- Wire-format parser bugs (`aura-protocol`) — fuzz harnesses live under `crates/aura-protocol/fuzz/`.
- Privilege escalation in admin/verification flows.

Out of scope (or report upstream):

- **Metadata leakage** that the protocol explicitly does not protect — see the threat model in `README.md` and `docs/08_security_review.md`. The server *needs* to see who is connected and to which channel in order to route ciphertext.
- **Endpoint compromise.** No protocol can save a conversation if a participant's machine is owned.
- **Denial of service** by sheer volume (Aura mitigates several DoS vectors but cannot guarantee availability against a determined attacker).
- **Vulnerabilities in third-party dependencies** (`ring`, `quinn`, `openmls`, `opus`, etc.) — please report upstream. If a dep CVE materially affects Aura, we will issue our own advisory linking to the upstream fix.
- Issues that require physical access or root on the user's own machine.

## Supported versions

Pre-alpha: only `main` is supported. There are no LTS branches and no backported fixes. Tagged releases (`v0.1.0-alpha` and beyond) will note the supported lifetime when they are cut.

## Known issues

The in-tree security review at [`docs/08_security_review.md`](docs/08_security_review.md) lists known open findings. Reports duplicating those will be acknowledged but de-prioritized; we already know.
