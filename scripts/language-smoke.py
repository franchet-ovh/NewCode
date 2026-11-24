#!/usr/bin/env python3
"""Exercise the language addon bridge against real or deterministic fake servers."""
import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
FAKE = r'''
import json, sys, os
if os.environ.get("FOCUS_SMOKE_SERVER_PID"):
    open(os.environ["FOCUS_SMOKE_SERVER_PID"],"w").write(str(os.getpid()))

def send(v):
    data = json.dumps(dict(jsonrpc='2.0', **v)).encode()
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n' % len(data)).encode()+data)
    sys.stdout.buffer.flush()
while True:
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line: sys.exit()
        if line == b'\r\n': break
        if line.lower().startswith(b'content-length:'): length = int(line.split(b':')[1])
    v = json.loads(sys.stdin.buffer.read(length))
    method = v.get('method')
    if method == 'initialize':
        send(dict(id=v['id'], result=dict(capabilities=dict(textDocumentSync=2, documentSymbolProvider=True, completionProvider={}, documentFormattingProvider=True))))
    elif method == 'textDocument/didOpen':
        doc = v['params']['textDocument']
        send(dict(method='textDocument/publishDiagnostics',params=dict(uri=doc['uri'],version=doc['version'],diagnostics=[dict(range=dict(start=dict(line=0,character=0),end=dict(line=0,character=1)),severity=1,source='fixture',message='type error')])))
    elif method == 'textDocument/completion': send(dict(id=v['id'],result=[dict(label='greeting',insertText='greeting')]))
    elif method == 'textDocument/formatting': send(dict(id=v['id'],result=[dict(range=dict(start=dict(line=0,character=0),end=dict(line=0,character=0)),newText=' ')]))
    elif method == 'textDocument/documentSymbol': send(dict(id=v['id'],result=[dict(name='greeting',kind=12,range=dict(start=dict(line=0,character=0),end=dict(line=0,character=1)),selectionRange=dict(start=dict(line=0,character=0),end=dict(line=0,character=1)))]))
    elif method == 'shutdown': send(dict(id=v['id'],result=None))
    elif method == 'exit': sys.exit()
'''


class Bridge:
    def __init__(self, addon, context, log, environment):
        self.child = subprocess.Popen([str(addon)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, start_new_session=True, env=environment)
        self.events = queue.Queue()
        self.settings = {}
        self.diagnostics = []
        self.session = 1
        self.context = context
        self.request_id = 10
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        for line in self.child.stdout:
            try:
                self.events.put(json.loads(line))
            except ValueError as error:
                self.events.put(error)
        self.events.put(RuntimeError('Addon stdout closed'))

    def send(self, method, params, request_id=None):
        message = dict(jsonrpc='2.0', method=method, params=params)
        if request_id is not None:
            message['id'] = request_id
        self.child.stdin.write((json.dumps(message)+'\n').encode())
        self.child.stdin.flush()

    def lsp(self, message):
        self.send('lsp.message', dict(generation=1, session=self.session, message=dict(jsonrpc='2.0', **message)))

    def next(self, timeout):
        event = self.events.get(timeout=timeout)
        if isinstance(event, Exception):
            raise event
        if event.get('method') == 'lsp.status' and event['params'].get('failed'):
            raise RuntimeError(event['params']['status'])
        if event.get('method') == 'lsp.message':
            message = event['params']['message']
            if 'method' in message and 'id' in message:
                if message['method'] == 'workspace/configuration':
                    result = []
                    for item in message['params']['items']:
                        section = item.get('section', '')
                        found = self.settings
                        if section:
                            for key in section.split('.'):
                                found = found.get(key, {}) if isinstance(found, dict) else None
                        result.append(found)
                    self.lsp(dict(id=message['id'], result=result))
                else:
                    self.lsp(dict(id=message['id'], result=None))
            if message.get('method') == 'textDocument/publishDiagnostics':
                self.diagnostics.extend(message['params']['diagnostics'])
        return event

    def wait(self, predicate, timeout=90):
        until = time.monotonic()+timeout
        while time.monotonic() < until:
            event = self.next(max(0.01, until-time.monotonic()))
            if predicate(event):
                return event
        raise TimeoutError('Language bridge timed out')

    def request(self, method, params):
        self.request_id += 1
        request_id = self.request_id
        self.lsp(dict(id=request_id, method=method, params=params))
        event = self.wait(lambda e: e.get('method') == 'lsp.message' and e['params']['message'].get('id') == request_id)
        response = event['params']['message']
        if 'error' in response:
            raise RuntimeError(response['error'])
        return response.get('result')

    def close(self):
        if self.child.poll() is None:
            self.send('shutdown', {})
            try:
                self.child.wait(timeout=4)
            except subprocess.TimeoutExpired:
                os.killpg(self.child.pid, 9)
                self.child.wait()
                raise AssertionError('Addon failed to shut down')
        self.child.stdin.close()
        self.child.stdout.close()


def smoke(language, root, args):
    workspace = root / language
    workspace.mkdir()
    if language == 'rust':
        (workspace/'Cargo.toml').write_text('[package]\nname="language_smoke"\nversion="0.1.0"\nedition="2021"\n')
        (workspace/'src').mkdir()
        file = workspace/'src/main.rs'
        text = 'fn greeting() -> i32 {1}\nfn main(){let wrong: i32 = "oops"; let value = greet;}\n'
        position = dict(line=1, character=text.splitlines()[1].index('greet')+5)
    else:
        file = workspace/'main.py'
        text = 'def greeting(name: str) -> str:\n    return name\nvalue: int = "wrong"\nanswer=greeting("x")\ngreet\n'
        position = dict(line=4, character=5)
    file.write_text(text)
    storage = (args.data_root / language) if args.data_root else workspace/'storage'
    storage.mkdir(parents=True, exist_ok=True)
    context = dict(generation=1, workspace=str(workspace), storage=str(storage), runtime=None, state={})
    environment = os.environ.copy()
    pid_file = root/f'{language}.pid'
    environment['FOCUS_SMOKE_SERVER_PID'] = str(pid_file)
    if args.fake_container:
        shim = root/'docker'
        shim.write_text("#!/usr/bin/env python3\nimport os,sys\na=sys.argv[2:]\nwhile a and a[0].startswith('-'):\n if a[0] in ('-w','-u'): a=a[2:]\n else: a=a[1:]\na=a[1:]\nos.execvp(a[0],a)\n")
        shim.chmod(0o755)
        environment['PATH'] = str(root)+os.pathsep+environment['PATH']
        context['runtime'] = dict(host_workspace=str(workspace),container_id='fixture',workspace_folder=str(workspace),remote_user=None)
    command = args.rust_server if language == 'rust' else args.python_server
    command_args = []
    if args.fake:
        fake = root/'fake.py'
        fake.write_text(FAKE)
        command, command_args = 'python3', [str(fake)]
    if args.fake_provision and language == 'rust':
        inventory = root/'rust-components.json'
        tool = """#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
inventory=Path(INVENTORY)
name=Path(sys.argv[0]).name
if name=='rustup':
 if FAIL: sys.exit(1)
 inventory.write_text(json.dumps(sys.argv[1:])); sys.exit(0)
if '--version' in sys.argv: sys.exit(0 if inventory.exists() else 1)
os.execv(sys.executable,[sys.executable,FAKE])
""".replace('INVENTORY',repr(str(inventory))).replace('FAIL',repr(args.fake_install_failure)).replace('FAKE',repr(str(fake)))
        for name in ('rustup','rust-analyzer','rustfmt','cargo'):
            shim=root/name
            shim.write_text(tool)
            shim.chmod(0o755)
        environment['PATH'] = str(root)+os.pathsep+environment['PATH']
        command, command_args = None, []
    addon = args.binary_dir.resolve()/f'focus-addon-lsp-{language}'
    with (root/f'{language}.log').open('wb') as log:
        bridge = Bridge(addon, context, log, environment)
        try:
            bridge.send('initialize', dict(protocol=1, language=1, context=context), 0)
            bridge.wait(lambda e: e.get('id') == 0)
            bridge.send('lsp.start', dict(generation=1, session=1, language=language, context=context, command=command, args=command_args))
            try:
                event = bridge.wait(lambda e: e.get('method') == 'lsp.started', timeout=330)
            except RuntimeError as error:
                if not args.fake_install_failure:
                    raise
                assert 'Rust requires' in str(error)
                assert not pid_file.exists(), 'Failed installation launched a server'
                print('rust: failed provisioning is reported without launching a server')
                return
            if args.fake_provision and language == 'rust':
                assert json.loads(inventory.read_text()) == ['component','add','rust-analyzer','rustfmt','clippy','rust-src']
            bridge.settings = event['params']['settings']
            result = bridge.request('initialize', dict(processId=None, rootUri=workspace.as_uri(), capabilities=dict(general=dict(positionEncodings=['utf-16']), workspace=dict(configuration=True), textDocument=dict(completion=dict(completionItem=dict(snippetSupport=False)))), initializationOptions=event['params'].get('initialization_options',bridge.settings)))
            assert result['capabilities'].get('completionProvider') is not None
            bridge.lsp(dict(method='initialized', params={}))
            settings = bridge.settings
            bridge.lsp(dict(method='workspace/didChangeConfiguration', params=dict(settings=settings)))
            uri = file.as_uri()
            bridge.lsp(dict(method='textDocument/didOpen', params=dict(textDocument=dict(uri=uri, languageId=language, version=1, text=text))))
            bridge.lsp(dict(method='textDocument/didSave', params=dict(textDocument=dict(uri=uri), text=text)))
            symbols = bridge.request('textDocument/documentSymbol', dict(textDocument=dict(uri=uri)))
            assert symbols, f'{language}: missing symbols'
            # Rust project loading can initially return an incomplete/empty list.
            until = time.monotonic()+60
            while True:
                completions = bridge.request('textDocument/completion', dict(textDocument=dict(uri=uri), position=position))
                items = completions.get('items', []) if isinstance(completions, dict) else (completions or [])
                if any('greeting' in item['label'] for item in items):
                    break
                assert time.monotonic() < until, f'{language}: missing greeting completion'
                time.sleep(0.2)
            edits = bridge.request('textDocument/formatting', dict(textDocument=dict(uri=uri), options=dict(tabSize=4, insertSpaces=True)))
            assert edits, f'{language}: missing formatting changes'
            if not bridge.diagnostics:
                bridge.wait(lambda e: e.get('method') == 'lsp.message' and e['params']['message'].get('method') == 'textDocument/publishDiagnostics' and e['params']['message']['params']['diagnostics'])
            assert any(d.get('severity',1) == 1 for d in bridge.diagnostics), f'{language}: no type errors'
            if not args.fake and language == 'python':
                assert any(d.get('source') == 'mypy' for d in bridge.diagnostics), 'Missing mypy type diagnostics'
            bridge.send('lsp.stop',dict(generation=1,session=1))
            bridge.wait(lambda e:e.get('method')=='lsp.stopped',timeout=4)
            if args.fake:
                try:
                    os.kill(int(pid_file.read_text()),0)
                except ProcessLookupError:
                    pass
                else:
                    raise AssertionError('Language server survived stop acknowledgement')
            assert file.read_text() == text, 'Language requests modified files' 
            print(f'{language}: completion, type diagnostics, formatting, symbols, stop acknowledgement passed')
        except Exception:
            log.flush()
            print((root/f'{language}.log').read_text(errors='replace')[-12000:])
            raise
        finally:
            bridge.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary-dir', type=Path, default=ROOT/'target/debug')
    parser.add_argument('--fake', action='store_true')
    parser.add_argument('--fake-container', action='store_true')
    parser.add_argument('--fake-provision', action='store_true')
    parser.add_argument('--fake-install-failure', action='store_true')
    parser.add_argument('--rust-server')
    parser.add_argument('--python-server')
    parser.add_argument('--data-root', type=Path)
    parser.add_argument('--language', choices=['rust','python'], action='append')
    args = parser.parse_args()
    if args.fake_provision and not args.fake:
        parser.error("--fake-provision requires --fake")
    if args.fake_install_failure and not args.fake_provision:
        parser.error("--fake-install-failure requires --fake-provision")
    if args.fake_container and not args.fake:
        parser.error("--fake-container requires --fake")
    with tempfile.TemporaryDirectory(prefix='focus-language-smoke-') as directory:
        root = Path(directory)
        for language in args.language or ['rust','python']:
            smoke(language, root, args)


if __name__ == '__main__':
    main()
