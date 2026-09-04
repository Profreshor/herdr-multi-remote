# herdr

> **Downstream fork — Profreshor/herdr-multi-remote.** This build keeps the
> `herdr` executable, configuration, and artifact names for compatibility, but
> publishes and updates from this repository only. It is stable-channel-only
> today; preview releases are not provided. Upstream Herdr remains at
> [herdrdev/herdr](https://github.com/herdrdev/herdr).

Status: actively maintained downstream fork; release automation and direct
install/update paths are fork-owned. Package-manager distribution is not
provided by this fork.

## What this fork adds

One client can operate its local Herdr server and several independent Herdr
servers over SSH. Each machine keeps ownership of its PTYs, processes,
workspaces, and agents; the client groups them by machine and routes each action
to the owning server. A disconnected remote does not take healthy servers down.

Add normal SSH hosts or aliases to Herdr's existing config:

```toml
[remotes.linux]
host = "dev@linux.example.com"

[remotes.mac]
host = "dev@mac.example.com"
```


<p align="center">
  <img src="assets/logo.png" alt="herdr" width="100" />
</p>

<p align="center">
  <a href="https://github.com/Profreshor/herdr-multi-remote">fork repository</a> · <a href="#install">install</a> · <a href="https://herdr.dev/docs/quick-start/">upstream quick start</a> · <a href="https://herdr.dev/docs/">upstream docs</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/Profreshor/herdr-multi-remote/releases"><img src="https://img.shields.io/github/v/release/Profreshor/herdr-multi-remote?label=release&labelColor=333333&color=666666" alt="latest stable release" /></a>
</p>

---

https://github.com/user-attachments/assets/043ec09f-4bdd-41d5-aee0-8fda6b83e267

**the runtime your coding agents live on.**

- **always running** — herdr is a background server; the terminals live inside it. close the lid, drop the network, or restart the machine; agents keep working and sessions come back. reattach from any terminal, or over ssh.
- **never hunt for the stuck one** — every pane is marked working, blocked, or idle. when an agent stops and needs an answer, herdr says so.
- **agent-native** — agents drive herdr through the cli and socket api: they can spawn panes, prompt each other, and wait until another agent is genuinely blocked. [agent skill →](https://herdr.dev/docs/agent-skill/)
- **runs what you already run** — claude code, codex, cursor, opencode, grok and the rest. herdr doesn't wrap or replace them; it owns their terminals.
- **keyboard and mouse, both first-class** — tmux-style prefix keys *and* click, drag, split. pick per moment, not per tool.
- **plugins** — extend panes and workflows. [browse the marketplace →](https://herdr.dev/plugins/)
- **one rust binary, no electron** — runs in whatever terminal you already use.

---

## install

```bash
curl -fsSL https://raw.githubusercontent.com/Profreshor/herdr-multi-remote/master/distribution/install.sh | sh
```

Windows: `powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/Profreshor/herdr-multi-remote/master/distribution/install.ps1 | iex"` · cmd: `curl.exe -fsSLo install.cmd https://raw.githubusercontent.com/Profreshor/herdr-multi-remote/master/distribution/install.cmd && install.cmd && del install.cmd` · [binaries](https://github.com/Profreshor/herdr-multi-remote/releases)

Fork defaults are stable-only and use the existing Herdr configuration format. For example:

```toml
[update]
channel = "stable"
```

then start it where the work lives:

```bash
herdr
```

run your agents, split panes, walk away. `ctrl+b q` detaches, `herdr` reattaches. [quick start →](https://herdr.dev/docs/quick-start/)

## docs

Upstream Herdr's documentation covers the shared runtime and interface at [herdr.dev/docs](https://herdr.dev/docs/): [quick start](https://herdr.dev/docs/quick-start/) · [concepts](https://herdr.dev/docs/concepts/) · [supported agents](https://herdr.dev/docs/agents/) · [keyboard](https://herdr.dev/docs/keyboard/) · [configuration](https://herdr.dev/docs/configuration/) · [session state](https://herdr.dev/docs/session-state/) · [remote](https://herdr.dev/docs/persistence-remote/) · [integrations](https://herdr.dev/docs/integrations/) · [plugins](https://herdr.dev/docs/plugins/) · [socket api](https://herdr.dev/docs/socket-api/)

## thanks

every past sponsor and backer is listed in [SPONSORS.md](./SPONSORS.md) — thank you 🐑

## agent instructions

if you are an ai agent helping with this repository, read [`AGENTS.md`](./AGENTS.md) before making changes and read [`CONTRIBUTING.md`](./CONTRIBUTING.md) before opening issues or PRs.

## development

```bash
git clone https://github.com/Profreshor/herdr-multi-remote
cd herdr-multi-remote
cargo build --release

just test        # unit tests
just check       # formatting, tests, and maintenance checks
```

## license

Herdr is licensed under the [Apache License 2.0](LICENSE).
