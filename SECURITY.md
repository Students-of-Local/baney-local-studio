# Security

## Reporting a vulnerability

Please report security problems privately through GitHub: open the [Security tab](https://github.com/Students-of-Local/baney-local-studio/security) of this repository and choose **Report a vulnerability**. Do not open a public issue for a security problem.

Include the version and channel from Settings or `local-studio --version`, the platform, and the steps that show the problem. You will get a reply within seven days.

## Scope

In scope: the macOS, Linux and iPhone apps, the Rust core, the local-LLM controller, `local-studio serve` and the remote protocol, and the plugin installer and grants.

Problems in Eukhe, the agent engine, belong to [gildrb/eukhe](https://github.com/gildrb/eukhe). Problems in a plugin belong to that plugin's repository.

## What Local Studio does not protect against

Agents run code with your user's permissions; the app is not a sandbox. Every capability widens what a plugin can do. A plugin you grant `accessibility`, `synthetic-input` or `screen-capture` can operate your computer; `site-secrets` lets it fill your saved secrets into sites; `native-host` lets it register a helper a browser extension can talk to. Grant only what you would give a person sitting at your keyboard.
