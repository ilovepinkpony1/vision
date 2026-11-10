"""bin/app-token: a JWT signed with RS256 by openssl, and an installation token of a GitHub App."""

import base64
import json
import os
import subprocess
import time
import unittest

from support import BIN, AdapterTest

TOKEN = "ghs_fakeinstallationtoken0123"


def unb64(text):
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


class AppToken(AdapterTest):
    def setUp(self):
        super().setUp()
        self.keys = os.path.join(self.tmp, "keys")
        os.makedirs(self.keys)
        self.key = os.path.join(self.keys, "app.pem")
        self.public = os.path.join(self.keys, "app.pub")
        subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", self.key],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        subprocess.run(["openssl", "pkey", "-in", self.key, "-pubout", "-out", self.public], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        with open(self.key) as handle:
            self.key_text = handle.read()
        self.claims = []
        self.server.route("POST", "/app/installations/99/access_tokens", self.issue)

    def issue(self, req):
        auth = req["headers"].get("authorization", "")
        if not auth.startswith("Bearer "):
            return 401, {"message": "no JWT"}
        header, payload, signature = auth[len("Bearer "):].split(".")
        sig = os.path.join(self.keys, "sig-%d.bin" % len(self.claims))
        with open(sig, "wb") as handle:
            handle.write(unb64(signature))
        check = subprocess.run(["openssl", "dgst", "-sha256", "-verify", self.public, "-signature", sig],
                               input=("%s.%s" % (header, payload)).encode(), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.claims.append((json.loads(unb64(header)), json.loads(unb64(payload)), check.returncode == 0))
        if check.returncode != 0:
            return 401, {"message": "A JSON web token could not be decoded"}
        return 201, {"token": TOKEN, "expires_at": "2026-11-10T13:00:00Z"}

    def app_token(self, *args):
        proc = subprocess.run([os.path.join(BIN, "app-token"), "--app-id", "123", "--installation-id", "99",
                               "--api", self.server.url] + list(args),
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env(secrets={}), timeout=60)
        out, err = proc.stdout.decode(), proc.stderr.decode()
        for part in self.key_text.splitlines()[1:-1]:
            self.assertNotIn(part, out + err, "the key is never printed")
        return proc.returncode, out, err

    def assert_good_jwt(self):
        header, claims, verified = self.claims[-1]
        self.assertTrue(verified, "openssl verifies the RS256 signature with the public key")
        self.assertEqual(header, {"alg": "RS256", "typ": "JWT"})
        self.assertEqual(claims["iss"], 123)
        now = int(time.time())
        self.assertLessEqual(claims["iat"], now)
        self.assertGreater(claims["exp"], now)
        self.assertLessEqual(claims["exp"] - claims["iat"], 600)

    def test_a_key_file(self):
        code, out, err = self.app_token("--key-file", self.key)
        self.assertEqual((code, out, err), (0, TOKEN + "\n", ""))
        self.assert_good_jwt()

    def test_a_key_command_leaves_no_file(self):
        before = sorted(os.listdir(self.scratch))
        code, out, err = self.app_token("--key-command", "cat '%s'" % self.key)
        self.assertEqual((code, out, err), (0, TOKEN + "\n", ""))
        self.assert_good_jwt()
        self.assertEqual(sorted(os.listdir(self.scratch)), before, "the key never lands in a file")
        request = self.server.calls("POST", "/app/installations/99/access_tokens")[-1]
        self.assertEqual(request["headers"]["accept"], "application/vnd.github+json")
        self.assertEqual(request["headers"]["x-github-api-version"], "2022-11-28")

    def test_failures_name_the_problem_and_never_the_key(self):
        code, out, err = self.app_token("--key-command", "exit 4")
        self.assertEqual((code, out), (1, ""))
        self.assertIn("the key command failed with exit code 4", err)
        bad = os.path.join(self.keys, "bad.pem")
        with open(bad, "w") as handle:
            handle.write("-----BEGIN PRIVATE KEY-----\nnot a key\n-----END PRIVATE KEY-----\n")
        code, out, err = self.app_token("--key-file", bad)
        self.assertEqual((code, out), (1, ""))
        self.assertIn("openssl could not sign", err)
        other = os.path.join(self.keys, "other.pem")
        subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", other],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        code, out, err = self.app_token("--key-file", other)
        self.assertEqual((code, out), (1, ""))
        self.assertIn("make an installation token (POST /app/installations/99/access_tokens) answered HTTP 401", err)
        self.assertNotIn("Bearer", err)
        code, out, err = self.app_token("--key-file", self.key, "--api", "http://api.example.com")
        self.assertEqual(code, 1)
        self.assertIn("https://", err)


if __name__ == "__main__":
    unittest.main()
