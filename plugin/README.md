# planvil plugin for Claude Code

This plugin adds the `/planvil` skill and the hooks that enforce planvil's feature workflow:
Feature → Audit → Plan → Decide → Implement → Validate → Repair → Done. Before the plan gates
pass, source edits are denied. The agent is sent back when it tries to stop on an unfinished
run. Completion requires recorded, current verification evidence.

## Install

1. Install the `planvil` binary. The hooks call it, so it must be on your `PATH`:

   ```sh
   curl -LsSf https://github.com/benpsk/planvil/releases/latest/download/planvil-installer.sh | sh
   ```

2. Install the plugin in Claude Code:

   ```
   /plugin install planvil --marketplace benpsk/planvil
   ```

   Or run `planvil install`, which registers the same plugin with the hooks pointing at the
   binary's absolute path.

See the main README for usage and configuration: https://github.com/benpsk/planvil
