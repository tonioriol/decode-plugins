#!/usr/bin/env bash
# Real CLI transcript shapes; successful mutations must gate, failures must not.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root="$(git -C "$here" rev-parse --show-toplevel)/.agent-runs/native-fixtures"
mkdir -p "$root"
python3 - "$here" "$root" <<'PY'
import datetime, json, os, pathlib, subprocess, sys, tempfile
here, root = map(pathlib.Path, sys.argv[1:])
hook = pathlib.Path(os.environ.get('MEMORY_HOOK_UNDER_TEST', str(here / 'flush-hooks.sh')))
base = 1790000000
folder = '20261010080000-feature'
passed = failed = 0
for client in ('claude', 'codex'):
 for action in ('edit', 'commit'):
  for success in (True, False):
   ws = pathlib.Path(tempfile.mkdtemp(prefix=f'{client}-{action}-', dir=root))
   notes = ws / 'memory' / folder
   notes.mkdir(parents=True)
   (notes / 'context.md').write_text('seed\n')
   def git(*args, at=0):
    env = dict(os.environ, GIT_AUTHOR_DATE=f'@{base+at}', GIT_COMMITTER_DATE=f'@{base+at}', GIT_EDITOR='true')
    return subprocess.run(['git', '-C', str(ws), '-c', 'user.name=test', '-c', 'user.email=test@test', *args], env=env, check=True, capture_output=True)
   git('init', '-q'); git('add', 'memory/'+folder+'/context.md'); git('commit', '-qm', 'seed')
   stamp = datetime.datetime.fromtimestamp(base+10, datetime.timezone.utc).isoformat()
   command = 'git commit -qm code'
   if client == 'claude':
    name, params = ('Write', {'file_path':'src/a.ts'}) if action == 'edit' else ('Bash', {'command':command})
    call = {'type':'assistant','timestamp':stamp,'message':{'role':'assistant','content':[{'type':'tool_use','id':'c1','name':name,'input':params}]}}
    result = {'type':'user','timestamp':stamp,'message':{'role':'user','content':[{'type':'tool_result','tool_use_id':'c1','is_error':not success,'content':'File created successfully' if action == 'edit' else ('[main 123abcd] code' if success else 'Exit code 1')}]}}
   else:
    if action == 'edit':
     payload = {'type':'custom_tool_call','call_id':'c1','name':'apply_patch','input':'*** Begin Patch\n*** Add File: src/a.ts\n+hi\n*** End Patch'}
     output = 'Success. Updated the following files:\nA src/a.ts\n' if success else 'Failed to apply patch'
     kind = 'custom_tool_call_output'
    else:
     payload = {'type':'function_call','call_id':'c1','name':'exec_command','arguments':json.dumps({'cmd':command})}
     output = 'Process exited with code '+('0' if success else '1')+'\nFinal output:\n'
     kind = 'function_call_output'
    call = {'timestamp':stamp,'type':'response_item','payload':payload}
    result = {'timestamp':stamp,'type':'response_item','payload':{'type':kind,'call_id':'c1','output':output}}
   transcript = ws/'transcript.jsonl'
   transcript.write_text(json.dumps(call)+'\n'+json.dumps(result)+'\n')
   request = {'session_id':'root','cwd':str(ws),'transcript_path':str(transcript),'hook_event_name':'Stop'}
   env = dict(os.environ, XDG_STATE_HOME=str(ws/'state'))
   def run():
    p=subprocess.run([str(hook)],input=json.dumps(request),text=True,capture_output=True,env=env,check=True)
    return json.loads(p.stdout) if p.stdout else {}
   def check(label, actual, expected):
    global passed, failed
    if actual == expected: passed+=1; print('ok  ',label)
    else: failed+=1; print('FAIL',label,repr(actual),'expected',repr(expected))
   label=f'{client} {action} {"success" if success else "failure"}'
   check(label+' gates only confirmed work',run().get('decision'), 'block' if success else None)
   check(label+' Stop does not loop',run(),{})
   request['hook_event_name']='PreCompact'
   if client == 'codex': request['turn_id']='turn-1'
   out=run()
   check(label+' compact uses native decision',out.get('continue') if client=='codex' else out.get('decision'), (False if client=='codex' else 'block') if success else None)
   if success:
    # A native edit naming the actual notes folder plus its real git commit clears the gate.
    if client=='claude':
     call['message']['content'][0]={'type':'tool_use','id':'notes','name':'Write','input':{'file_path':'memory/'+folder+'/context.md'}}
    else:
     call['payload']={'type':'custom_tool_call','call_id':'notes','name':'apply_patch','input':'*** Begin Patch\n*** Update File: memory/'+folder+'/context.md\n@@\n-seed\n+flushed\n*** End Patch'}
    transcript.write_text(transcript.read_text()+json.dumps(call)+'\n')
    (notes/'context.md').write_text('flushed\n');git('add','memory/'+folder+'/context.md');git('commit','-qm','notes',at=20)
    check(label+' committed notes release compact',run(),{})
print(f'{passed} passed, {failed} failed')
raise SystemExit(bool(failed))
PY
