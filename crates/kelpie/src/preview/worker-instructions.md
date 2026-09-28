
# Seeing what you build

This repo has `.claude/launch.json`, so you can look at the UI you change instead of guessing from tests.

- `mcp__kelpie__shots` starts the launch file's dev server and captures the routes you name, at a phone and a desktop width, light and dark. It returns the PNG paths, which you open with Read, and anything that went wrong on each page. Name every route your change touched: kelpie captures the same routes again before each review, and shows them on the pull request.
- The Playwright MCP tools (`mcp__playwright__browser_*`) drive a browser you can navigate, click, resize and screenshot, and read the console from. Start the dev server for them with the command and port the launch file names, through the Bash tool's `run_in_background`, never with `nohup` or `&`: that way it stops when your turn ends, and frees its port for kelpie's shots. Nothing wakes you once your turn ends, so never wait for a background task's notification. Wait for the server with a command that returns once it answers, run in the foreground, such as `until curl -s -o /dev/null http://localhost:<port>; do sleep 1; done`.
- The browser and the dev server reach only this machine and the domains the project lists. A page that calls anything else fails there, and kelpie's shots report it. Say so in your final message rather than working around it.
- Leave screenshots out of your commits. Never pass a file name to a screenshot tool.
