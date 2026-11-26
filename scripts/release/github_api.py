"""Small GitHub CLI adapter with explicit HTTP errors and JSON request bodies."""
import json
import re
import subprocess


class GitHubError(RuntimeError):
    def __init__(self, message):
        super().__init__(message)
        match = re.search(r'HTTP (\d{3})', message)
        self.status = int(match[1]) if match else None


def gh(*args, input_data=None):
    result = subprocess.run(['gh', *args], input=input_data, text=True, capture_output=True)
    if result.returncode:
        raise GitHubError(result.stderr.strip() or result.stdout.strip() or f'exit code {result.returncode}')
    return result.stdout.strip()


def api(repo, endpoint, data=None, missing=False):
    args = ['api', f'repos/{repo}/{endpoint}']
    try:
        if data is None:
            result = gh(*args)
        else:
            result = gh(*args, '--method', 'POST', '--input', '-', input_data=json.dumps(data))
    except GitHubError as error:
        if missing and error.status == 404:
            return None
        raise
    return json.loads(result)
