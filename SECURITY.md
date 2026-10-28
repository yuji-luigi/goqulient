# Security policy

GoQulient connects to databases with your credentials, so security reports matter to us. Thank you for taking the time.

## Reporting a vulnerability
Please **don't open a public issue**. Report it privately through GitHub:
[Security → Report a vulnerability](https://github.com/yuji-luigi/goqulient/security/advisories/new).

Include the version (extension and platform), what you did, what happened and, if you can, a minimal reproduction.
Never send real credentials or production data; the throwaway Docker databases (`scripts/dev.sh up`) are enough to
reproduce almost anything. We aim to reply within 7 days; fixes are released as soon as they're ready and
credited in the advisory unless you prefer otherwise.

## What counts
Anything that breaks one of the promises in [AGENTS.md](AGENTS.md#safety), for example:
- a password, key passphrase or full connection string reaching a log, the output channel, an error message, a tree
  label or tooltip, webview HTML, settings, the process's argv or environment, or any unencrypted file;
- a saved password handed to a different server than the one it was typed for;
- a **read-only bypass**: a write that succeeds on a connection marked read-only (including `.env` connections, which
  are read-only by default);
- an untrusted workspace (Workspace Trust) that can make GoQulient read its `.env` files or workspace connection
  settings, or point `goqulient.enginePath` at another program;
- a webview that loads remote content, runs injected script or escapes its Content Security Policy;
- a malicious server response that crashes VS Code, makes the engine use unbounded memory or ignore its deadlines, or
  runs code.

Not in scope: an attacker who already controls your machine or your VS Code user settings, and problems in a database
server itself.

## Supported versions
Only the latest release gets security fixes while GoQulient is below 1.0.
