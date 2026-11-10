"""The scripts of the adapter for Jira Cloud and GitHub (design section 11).

The modules:

- core: the call loop of a script, the configuration, the secrets, git, and HTTP.
- adf: Atlassian Document Format to Markdown and back.
- jira, forge: the two clients.
- observe, act, jobs: the three scripts of the contract.
- app_token: the helper that makes a token of a GitHub App.

Standard library only, Python 3.9 or newer, macOS and Linux.
"""
