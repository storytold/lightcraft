# Monitoração de PRs — LightCraft

## PRs a acompanhar

| PR | Branch | Head SHA | Base | Estado CI | Link |
|----|--------|----------|------|-----------|------|
| #2338 | feat/keymap-classic-profile | `0cf5c09a` | main (`5194cc20`) | gate pending | https://github.com/storytold/lightcraft/pull/2338 |
| #2344 | feat/crop-lightroom-canvas | `dc88c26e` | main (`5194cc20`) | gate pending | https://github.com/storytold/lightcraft/pull/2344 |

## Última ação

- 2026-10-30: rebase de ambos os branches sobre `5194cc20` + push `--force-with-lease`.
- Comentários postados em ambos os PRs avisando que estão rebaseados e verdes localmente.
- CI disparado novamente após o force push; `gate / gate (ubuntu-24.04)` pending em ambos.

## O que fazer quando houver atualização

1. **CI verde**: revisar body do PR (já atualizado com evidências de validação) e aguardar review/merge.
2. **CI falhou (flake FreeBSD `control_server::tests::persistent_requests_and_slow_replies_outlive_the_idle_read_timeout`)**: retrigger via `gh pr close <n>` + `gh pr reopen`.
3. **Comentário de reviewer**: responder nesta sessão ou criar novo plano de ação.
4. **Review solicitado**: aplicar mudanças solicitadas, rebasear se main avançou, push com `--force-with-lease`, atualizar body.

## Comandos úteis

```powershell
# Ver status dos checks
gh pr checks 2338 --repo storytold/lightcraft
gh pr checks 2344 --repo storytold/lightcraft

# Ver comentários recentes
gh pr view 2338 --repo storytold/lightcraft --json comments
gh pr view 2344 --repo storytold/lightcraft --json comments

# Retrigger em caso de flake
gh pr close 2338 --repo storytold/lightcraft && gh pr reopen 2338 --repo storytold/lightcraft
gh pr close 2344 --repo storytold/lightcraft && gh pr reopen 2344 --repo storytold/lightcraft
```

## Contexto

- Ambos os PRs estão `MERGEABLE`.
- #2344 (Lightroom canvas) depende de #2334 (já merged).
- #2338 (Classic keymap) é independente.
- Falha pré-existente no Windows a ignorar: `tests_import::recursive_discovery_includes_sony_sr2_and_srf_case_insensitively`.
- Chave de push: `git -c credential.helper= -c "credential.helper=!gh auth git-credential" push fork <branch> --force-with-lease=<branch>:<old-sha>`.
