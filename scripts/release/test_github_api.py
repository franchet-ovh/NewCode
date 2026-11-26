"""Do not mistake authentication or transport errors for an unused release tag."""
import json
import subprocess
import unittest
from unittest.mock import patch

import github_api


class GitHubApiTests(unittest.TestCase):
    def test_only_404_means_missing(self):
        for status in (403, 404, 429, 500):
            with patch.object(github_api, 'gh', side_effect=github_api.GitHubError(f'GitHub failed (HTTP {status})')):
                if status == 404:
                    self.assertIsNone(github_api.api('owner/repo', 'releases/tags/addon_fixture_1.0.0', missing=True))
                else:
                    with self.assertRaises(github_api.GitHubError):
                        github_api.api('owner/repo', 'releases/tags/addon_fixture_1.0.0', missing=True)

    def test_json_bodies_preserve_literal_text_without_shell_interpolation(self):
        body = {'content': 'newlines\nquotes " and $(literal) `literal`'}
        with patch.object(github_api.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '{"sha":"commit"}', '')) as run:
            self.assertEqual(github_api.api('owner/repo', 'git/commits', body), {'sha': 'commit'})
            args, kwargs = run.call_args
            self.assertEqual(args[0][-2:], ['--input', '-'])
            self.assertEqual(json.loads(kwargs['input']), body)
            self.assertNotIn('shell', kwargs)


if __name__ == '__main__':
    unittest.main()
