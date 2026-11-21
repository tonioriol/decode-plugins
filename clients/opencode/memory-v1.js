import { spawnSync } from "node:child_process"
import { existsSync } from "node:fs"
import { join } from "node:path"
import { fileURLToPath } from "node:url"

const hooks = fileURLToPath(new URL("../../plugins/memory/hooks/", import.meta.url))

function run(script, payload) {
  const result = spawnSync("bash", [join(hooks, script)], {
    input: JSON.stringify(payload), encoding: "utf8", timeout: 10000,
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

    const prompt = (output.parts || []).filter(p => p.type === "text" && !p.synthetic).map(p => p.text).join("\n")
    const start = { cwd: directory, session_id: sessionID, hook_event_name: "SessionStart", source: "startup", prompt }
    const guidance = run("session-start-index.sh", start)
    if (guidance) output.message.system = [output.message.system, guidance].filter(Boolean).join("\n\n")
  },
})
