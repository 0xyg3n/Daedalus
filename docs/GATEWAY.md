# Provider gateway settings

Open **Settings → Connect an LLM proxy gateway** to configure an existing server.
Daedalus pairs Claude with Anthropic Messages and Codex with OpenAI Responses.
The endpoint, model and private credential file remain explicit. Other harnesses
retain their own configuration. Gateway credentials and billing are separate from
native harness sign-in.

The editor prefills saved fields; Ctrl+U clears a field. Invalid input keeps the
other draft fields. Choosing a provider preserves the current enable state. The
enable-state step and final review show whether the saved gateway will be used.
Enable and Disable also operate on an existing configuration directly. Changes
apply to future launches of the matching harness; saving does not launch a client
or contact the gateway.

The CLI exposes the same settings:

```sh
daedalus gateway config show --config-dir /abs/instance
daedalus gateway config init --config-dir /abs/instance --provider claude --endpoint https://gateway.example.invalid --model explicit-model-id --credential-file /abs/private/gateway-token.txt
daedalus gateway config init --config-dir /abs/instance --provider claude --endpoint https://gateway.example.invalid --model explicit-model-id --credential-file /abs/private/gateway-token.txt --apply
daedalus gateway config set --config-dir /abs/instance --key enabled --value true --apply
```

`init` starts disabled unless `--enabled true` is supplied. `set` edits `enabled`,
`provider`, `endpoint`, `model` or `credential_file`. Provider selects the supported
format/harness pair together. The [example](../config/gateway.example.json) lists
all saved fields; version 1 is the supported structural schema.

Each change previews by default. `--apply` uses the frozen reviewed bytes,
rejects a changed file or retired instance, and preserves prior bytes in a private
backup. Configuration is limited to 16 KiB. Endpoint URLs require HTTPS or
loopback HTTP without embedded credentials, query strings, fragments, surrounding
whitespace or control characters. Enabled saves validate the private credential
file. Reviews show its path and never contain its contents.

Native instance imports preserve the enable state, format, harness, endpoint and
model. `--credentials` selects copying the configured gateway credential into
the private destination `gateway-token.txt`. Without it, provision that file
before launching an enabled override; the enabled override remains selected.
Native harness sign-in files are not copied.

Final cutover preserves the destination's reviewed gateway selection and its
provisioned credential, without expanding source credential consent. Changed
destination configuration or credential bytes invalidate cutover review. Rollback
restores the destination backup under the existing activation rules. Launching
from a retired instance is refused.
