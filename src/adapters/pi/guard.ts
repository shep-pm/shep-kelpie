// Kelpie's checks on a fenced pi session: `kelpie confine` on every file
// write and edit, `kelpie guard` on every command. Each reads the call the
// way Claude Code's PreToolUse hooks hand it over, and exit code 2 refuses.
// Kelpie fills in CHECKS for each call. Anything that fails refuses.
import { spawnSync } from "node:child_process";
import { homedir } from "node:os";
import { resolve } from "node:path";

const CHECKS: { kelpie: string; confine: string[]; guard: string[] } = __CHECKS__;

// pi strips a leading `@`, expands `~` and folds odd spaces before it resolves a path.
function resolved(path: string, cwd: string): string {
	let p = path.replace(/[  -   　]/g, " ");
	if (p.startsWith("@")) p = p.slice(1);
	if (p === "~") p = homedir();
	else if (p.startsWith("~/")) p = homedir() + p.slice(1);
	return resolve(cwd, p);
}

function judged(args: string[], call: object): string | undefined {
	const run = spawnSync(CHECKS.kelpie, args, {
		input: JSON.stringify(call),
		encoding: "utf8",
	});
	if (run.status === 0) return undefined;
	const why = `${run.stderr ?? ""}${run.stdout ?? ""}`.trim();
	return why || `kelpie refused this call${run.error ? `: ${run.error.message}` : ""}`;
}

export default function (pi: any) {
	pi.on("tool_call", async (event: any, ctx: any) => {
		const cwd = ctx?.cwd ?? process.cwd();
		const input = event.input ?? {};
		let why: string | undefined;
		if (event.toolName === "bash") {
			why = judged(CHECKS.guard, {
				cwd,
				tool_name: "Bash",
				tool_input: { command: String(input.command ?? "") },
			});
		} else if (event.toolName === "write" || event.toolName === "edit") {
			why = judged(CHECKS.confine, {
				cwd,
				tool_name: event.toolName === "write" ? "Write" : "Edit",
				tool_input: { file_path: resolved(String(input.path ?? ""), cwd) },
			});
		} else if (!["read", "grep", "find", "ls"].includes(event.toolName)) {
			why = `kelpie runs no ${event.toolName} tool for a worker`;
		}
		if (why !== undefined) return { block: true, reason: why };
	});
}
