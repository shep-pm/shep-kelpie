# Fence: what a call reaches

The hosts a sandbox reaches beyond GitHub and the model. What a worker may write and run is in
fence.md, what it reads (dependency sources included) in fence-reads.md.

- Hosts: `allowed_domains`, and always the registries of the package managers the call's worktree keeps a lockfile or manifest for, in its root or a folder directly under it: npm and pnpm `registry.npmjs.org`, yarn `registry.yarnpkg.com` and `registry.npmjs.org`, bun `registry.npmjs.org`, cargo `crates.io`, `index.crates.io` and `static.crates.io`, go `proxy.golang.org` and `sum.golang.org`, uv and pip `pypi.org` and `files.pythonhosted.org`. One table, `crates/core/src/package_managers/mod.rs`, also gives the dev server's install its lockfiles, commands and caches and `doctor`'s `package registries` line its managers (`runner/dev_server/install/`, `src/doctor/registries.rs`). Only a call that works an item gets them (`Runner::worker_reach`); reviewers and the other roles keep `allowed_domains`. `crates/core/src/profile/mod.rs`
