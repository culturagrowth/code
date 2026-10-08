# Revisão — tarefa nº 19: comando de teste do fluxo de grupo (`npm run test:crew`)

- Branch: `gpt/crew-smoke` · commit revisado: `d871a2fa22a447f64a68a0dce6a03e75bf6bcce7` (base `9dc9283`)
- Implementado por: GPT · Revisado por: Claude · Data: 2026-10-08 · critério leve (decisão 19)
- Checagens refeitas pelo Claude num worktree próprio: `npm run typecheck` ok · `npm test` **375 passaram, 18 arquivos**.
  Rust não foi alterado. Não executei a ferramenta contra nenhum Worker (nem local aberto, nem remoto).

## Conferido
- Fluxo completo e correto: health → cadastro de dois dispositivos com chaves Ed25519 em memória → grupo → convite → entrada → membros →
  heartbeats e retrato com os dois → os dois ociosos no fim. Falha com exit code 1 e passo/código sanitizados.
- Assina exatamente como o Worker valida (`canonicalString` do próprio `src/auth.ts`, timestamps estritamente crescentes para não repetir assinatura).
- Não imprime chaves, assinaturas nem o código do convite; `redirect: "error"`, timeout de 15 s; HTTP sem TLS só em loopback.
- Os dados que ficam no D1 (dois cadastros, um grupo, um convite) estão documentados no README.

## Achados
Nenhum que impeça o uso. Observação para depois (não bloqueia): quando o Worker for publicado, rodar contra produção deixa dispositivos e grupos
"DuoClip teste" no D1 a cada execução; quando houver rota de remoção, vale limpar no fim.

## Veredito
**Aprovado.**
