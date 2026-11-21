import { spawnSync } from "node:child_process"
import { existsSync } from "node:fs"
import { join } from "node:path"
import { fileURLToPath } from "node:url"

const hooks = fileURLToPath(new URL("../../plugins/memory/hooks/", import.meta.url))

function run(script, payload) {
  const result = spawnSync("bash", [join(hooks, script)], {
    input: JSON.stringify(payload), encoding: "utf8", timeout: 5000,
  })
  return result.status === 0 ? result.stdout.trim() : ""
}

export const DecodeMemory = async ({ client, directory }) => ({
  "chat.message": async ({ sessionID }, output) => {
    if (!existsSync(join(directory, "memory"))) return
    const session = (await client.session.get({ path: { id: sessionID } })).data
    if (!session || session.parentID) return
    // OpenCode fires chat.message before persisting the user message. Unlike
    // session.created, this also lets us add context synchronously.
    const prior = (await client.session.messages({ path: { id: sessionID }, query: { limit: 1 } })).data
    if (!prior || prior.length) return

    const base = { cwd: directory, session_id: sessionID }
    const start = { ...base, hook_event_name: "SessionStart", source: "startup" }
    const guidance = run("session-start-index.sh", start)
    run("prompt-notes.sh", start)
    const prompt = output.parts.filter(p => p.type === "text" && !p.synthetic).map(p => p.text).join("\n")
    const hint = run("prompt-notes.sh", { ...base, hook_event_name: "UserPromptSubmit", prompt })
    const context = [guidance, hint && JSON.parse(hint).hookSpecificOutput.additionalContext].filter(Boolean).join("\n\n")
    if (context) output.message.system = [output.message.system, context].filter(Boolean).join("\n\n")
  },
})
