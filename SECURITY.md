# Security

## Reporting a problem

Please don't report a security problem in a public issue. Report it privately, through GitHub's
private vulnerability reporting: the "Report a vulnerability" button on this repository's
[Security tab](https://github.com/scornz/lo/security), which opens a
[private advisory](https://github.com/scornz/lo/security/advisories/new) that only you and the
maintainers can see.

Say what you found, how to reproduce it, and what it lets someone do. A pull request or repository
that shows it helps most.

Lo isn't released yet, so there are no supported versions: fixes land on `main`.

## In scope

Lo signs in to GitHub, copies other people's repositories and shows their code, so these matter
most:

- **Tokens.** How Lo stores GitHub tokens (the Keychain in release builds, a file only you can read
  in debug builds), hands them to git, and sends them to GitHub. A token reaching any other host,
  a log, any other file or a process's arguments is a bug.
- **Running a repository's code.** Lo never installs dependencies or runs build steps, and runs
  language servers only on repositories you trust, and on a pull request from a fork only if you
  trust that pull request. A pull request that gets Lo to run its code without that trust is a bug:
  through a language server, a repository's own settings or tasks files, its git config or
  attributes, or anything else Lo reads from it.
- **Writes to GitHub.** Lo writes to GitHub only to review and merge, and to change a pull
  request's base branch to form a stack. Anything that gets it to write more, or to act as a
  different account than the one shown, is a bug.
- **Other hosts.** Lo talks only to GitHub and to language servers' official download sources. A
  request to any other host, including Zed's servers, is a bug.
- **Content from GitHub.** Markdown, images and other content from a pull request that can do more
  than display.

## Out of scope

- Code a language server runs on a pull request you chose to trust, such as rust-analyzer running
  cargo under `--trust`. Trusting a pull request means you'd run its code.
- Problems in Zed's own code. Report those to [Zed](https://github.com/zed-industries/zed/security),
  and tell us if Lo is affected.
- Problems in GitHub itself. Report those through
  [GitHub's bug bounty](https://bounty.github.com).
