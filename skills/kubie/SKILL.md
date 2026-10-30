---
name: kubie
description: Use kubie to list Kubernetes contexts and run commands in a specific context non-interactively. Use when the user asks to inspect or operate on a Kubernetes cluster, switch kubectx, run kubectl/helm against a named cluster, or discover which clusters exist (local kubeconfigs plus cloud providers like DigitalOcean, GKE, EKS, AKS, Linode, Rancher).
---

# kubie for agents

`kubie` isolates kubeconfigs per command, so you never mutate the user's global
kubectl context. Two commands are agent friendly and fully non-interactive:

- `kubie list` - enumerate every known context.
- `kubie exec <context> <namespace> <command...>` - run a command in that context.

Never run `kubectl config use-context` or edit `~/.kube/config`; use `kubie exec`.

## Listing contexts

```bash
kubie list                # one context name per line when piped
kubie list --json         # name, source, cluster, server, namespace, provider, account
kubie list --local        # local kubeconfig files only, no cloud providers
kubie list --no-sync      # cached provider metadata only, never hits the network
```

`kubie ls` is an alias. Output is sorted. Provider-discovered clusters show
`"source": "provider"` in JSON along with `provider` and `account`.

Prefer `kubie list --json` when you need cluster endpoints or to distinguish
local from cloud contexts. Provider contexts can be selected with `provider/name`,
such as `rancher/my-cluster` or `digitalocean/my-cluster`. Use
`provider/account/name` when a provider has matching names in multiple accounts.
A recognized provider prefix reserves qualified syntax, and malformed qualified selectors
fail closed instead of falling back to local/fuzzy matching. `@id:<hex>` is reserved for opaque ID
selectors; copy the `selector` field from `kubie list --json` rather than constructing one.
Use `--local` for literal slash-containing local context names. Quote wildcard selectors, for example
`kubie exec 'rancher/my-cluster-*' default kubectl get pods`. Older bare context-history
entries remain bare until selected again by a qualified name. Same-name contexts across files
within one kubeconfig-provider account are still merged and may resolve to the first match. Put
them in separate provider accounts or rename the local contexts.
Prefer `kubie list --no-sync` in tight loops.

## Running commands

```bash
kubie exec my-cluster default kubectl get pods
kubie exec prod-us-east-1 kube-system kubectl get deploy -o json
kubie exec 'prod-*' default kubectl get nodes          # wildcard: runs per matching context
kubie exec 'rancher/my-cluster-*' default kubectl get pods # provider-qualified wildcard
kubie exec -e 'staging-*' default kubectl get ns       # stop at first failure
```

Rules:

- The namespace argument is **mandatory**; pass `default` if unsure.
- The context argument supports `*` and `?` wildcards. A wildcard may fan out
  across many clusters, so be deliberate.
- The command runs with `KUBECONFIG` pointed at a temporary single-context file.
  Exit code propagates from the child command.
- `--context-headers` controls the `CONTEXT => name` banner; disable it when
  parsing output (`--context-headers=never`).

## Caching behaviour

Resolution is cheapest-first and avoids network calls when possible:

1. Local kubeconfig contexts are matched first.
2. Cached cloud provider metadata (`~/.cache/kubie/providers/metadata.json`) is
   consulted next, and only the *matching* cluster's kubeconfig is downloaded.
3. A full provider sync happens only when nothing matched anywhere.

Downloaded kubeconfigs are cached for 24 hours in
`$XDG_CACHE_HOME/kubie/providers/configs/` (default `~/.cache/kubie/providers/configs/`).
Cache directories are owner-only (`0700`); files are atomically written with `0600`
permissions. Existing cache permissions are tightened before use. Legacy `/tmp`
provider caches are not reused and can be removed with `kubie clear-cache`.

Flags:

- `--no-sync` - use cached metadata only, never talk to provider APIs.
- `--local` - ignore providers entirely.

## Getting a kubeconfig path

For tools that need a real file (helm, k9s, terraform):

```bash
export KUBECONFIG="$(kubie export my-cluster default)"
```

This writes an isolated temp kubeconfig and prints its path. Delete it when done.

## Other commands

- `kubie info ctx` / `kubie info ns` - current kubie shell's context/namespace.
- `kubie lint` - scan kubeconfig files for problems.
- `kubie ctx` / `kubie ns` - interactive pickers. **Avoid these as an agent**;
  they spawn a shell or a TUI.

## Troubleshooting

- `kubie clear-cache` removes provider discovery metadata at
  `$XDG_CACHE_HOME/kubie/providers/metadata.json` (default
  `~/.cache/kubie/providers/metadata.json`) and downloaded provider kubeconfigs in
  `$XDG_CACHE_HOME/kubie/providers/configs` (default `~/.cache/kubie/providers/configs`),
  plus the legacy `/tmp/kubie-providers-<uid>/configs`. Configuration, original kubeconfigs,
  history, frecency, and active isolated sessions are preserved. It makes no
  provider API calls. The next normal sync/hydration repopulates the caches.
  Clearing is best-effort against concurrent syncs, so avoid running a sync at
  the same time. There are no flags or dry-run mode. It works even if kubie's
  configuration is malformed.
- `No context matching X` - run `kubie list` to see valid names; provider
  contexts often carry a provider or region prefix, such as `<provider>-<region>-`.
- Stale cluster list - run `kubie clear-cache`, then `kubie list` to rediscover providers.
