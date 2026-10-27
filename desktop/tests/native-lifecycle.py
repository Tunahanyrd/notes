#!/usr/bin/env python3
"""Start only our native binary with fresh offline data; terminate only its PID."""
import json, os, pathlib, signal, subprocess, tempfile, time, urllib.request
root=pathlib.Path(__file__).resolve().parents[2]
directory=pathlib.Path(tempfile.mkdtemp(prefix='notes-native-integration-'))
os.chmod(directory,0o700)
env=dict(os.environ,LINUX_NOTES_DATA_DIR=str(directory/'data'),LINUX_NOTES_OFFLINE='1')
log=directory/'native-stderr.log'
fd=os.open(log,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600)
process=subprocess.Popen([str(root/'desktop/src-tauri/target/debug/linux-notes-desktop')],env=env,stdout=subprocess.DEVNULL,stderr=fd)
os.close(fd)
owned_child=None
try:
    runtime=None
    for _ in range(300):
        if process.poll() is not None: raise RuntimeError('native launcher exited before readiness; private synthetic log retained')
        children=pathlib.Path(f'/proc/{process.pid}/task/{process.pid}/children').read_text().split()
        for child in children:
            args=pathlib.Path(f'/proc/{child}/cmdline').read_bytes().split(b'\0')
            if os.path.basename(args[0]) != b'notes-engine' or b'--runtime-file' not in args: continue
            owned_child=int(child)
            path=pathlib.Path(os.fsdecode(args[args.index(b'--runtime-file')+1]))
            try: runtime=json.loads(path.read_text())
            except (OSError,json.JSONDecodeError): continue
            assert runtime['pid']==owned_child
        if runtime: break
        time.sleep(.1)
    assert runtime, 'native engine readiness timeout'
    request=urllib.request.Request(f"http://127.0.0.1:{runtime['port']}/api/status",headers={'Authorization':'Bearer '+runtime['token']})
    with urllib.request.urlopen(request,timeout=5) as response: status=json.load(response)
    assert status['connected'] is False
    time.sleep(2)
    assert process.poll() is None
    print('Native Tauri window process and owned offline engine started; authenticated status verified.')
finally:
    if process.poll() is None:
        process.send_signal(signal.SIGTERM)
        try: process.wait(timeout=8)
        except subprocess.TimeoutExpired: process.kill();process.wait()
    if owned_child:
        for _ in range(80):
            state=pathlib.Path(f'/proc/{owned_child}/stat')
            if not state.exists() or state.read_text().split()[2]=='Z': break
            time.sleep(.1)
        else: raise RuntimeError('owned engine survived parent termination')
        print('Parent-death shutdown verified for the owned engine; no unrelated process touched.')
