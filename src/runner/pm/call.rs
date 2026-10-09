//! The project manager's call: its folder, files and fence
//!
//! It runs in its own folder, which holds a copy of the board as it stood
//! when it woke and its notes. Of kelpie's and the shepherd's homes it reads
//! that folder alone, and it never reads the checkout, `gh`'s config or the
//! credentials no worker reads. The rest of the machine stays readable, as
//! for a reviewer, since Claude Code reads its own files wherever they are.
//! It reaches no host but the model's, has only its read and file tools,
//! and writes only by adding to its notes, which a hook enforces.

use std::collections::BTreeMap;

use crate::ports::{AgentCall, Fence, Guard, PM_NOTES, Reach, Role, Session, Tools};
use crate::runner::Runner;
use crate::settings::PmAgent;

/// `gh`'s config, its token with it, which a worker reads to open its pull
/// request and the project manager never needs
const GH_CONFIG: &str = "~/.config/gh/**";

impl Runner {
    // The call, with its folder, notes, board, settings and prompt on disk.
    pub(super) fn pm_call(
        &self,
        agent: &PmAgent,
        session: Session,
        prompt: String,
    ) -> Result<AgentCall, String> {
        let folder = &self.paths.pm;
        let failed = |what: &std::path::Path, e: std::io::Error| {
            format!("cannot write {}: {}", what.display(), e.kind())
        };
        std::fs::create_dir_all(folder).map_err(|e| failed(folder, e))?;
        let notes = folder.join(PM_NOTES);
        if !notes.exists() {
            std::fs::write(&notes, "").map_err(|e| failed(&notes, e))?;
        }
        // A copy, so the board it reads holds still while it reads.
        let board = folder.join("board.md");
        match std::fs::copy(&self.paths.board, &board) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::write(&board, "").map_err(|e| failed(&board, e))?;
            }
            Err(e) => return Err(failed(&board, e)),
        }
        let instructions = self.paths.worker.join("pm-instructions.md");
        let text = agent.prompt.as_deref().unwrap_or_default();
        crate::runner::turn::write(&self.paths.worker, &instructions, text)?;
        self.prepared(AgentCall {
            role: Role::Pm,
            harness: agent.model.harness.clone(),
            issue: 0,
            model: agent.model.model.as_str().to_owned(),
            effort: agent.model.effort,
            session,
            cwd: folder.clone(),
            settings: self.paths.worker.join("pm-settings.json"),
            instructions: Some(instructions),
            prompt,
            plugin_dirs: Vec::new(),
            tools: Tools::Pm,
            lease: agent.limit.lease().cloned(),
            reach: self.pm_reach(),
        })
    }

    // Its folder to read and write; none of the homes beside it, the
    // checkout or `gh`'s token; no host but the model's.
    pub(super) fn pm_reach(&self) -> Reach {
        let folder = self.paths.pm.clone();
        let (shep, kelpie) = (&self.paths.shep_home, &self.paths.kelpie_home);
        let homes = [shep.clone()]
            .into_iter()
            .chain((!kelpie.starts_with(shep)).then(|| kelpie.clone()))
            .chain([self.settings.git.checkout.clone()])
            .map(|home| format!("{}/**", home.display()));
        let no_read = (crate::profile::CREDENTIALS.iter())
            .chain(&[GH_CONFIG])
            .map(|p| (*p).to_owned())
            .chain(homes)
            .collect();
        Reach {
            read: Vec::new(),
            fence: Some(Box::new(Fence {
                write: vec![folder.clone()],
                no_write: Vec::new(),
                no_read,
                read: vec![folder.clone()],
                hosts: Vec::new(),
                no_commands: Vec::new(),
                env: BTreeMap::new(),
                sockets: Vec::new(),
                guard: Guard {
                    kelpie: self.kelpie.clone(),
                    worktree: folder.clone(),
                    build: folder.clone(),
                    git_common_dir: folder,
                    folders: vec![kelpie.clone(), self.settings.git.checkout.clone()],
                    issues: None,
                },
                hooks: Vec::new(),
            })),
        }
    }
}
