# Revisão — tarefa nº 10: presença por grupo no Worker

- Branch: `gpt/worker-presenca` · commits revisados: `41fac5f..48c0f8a2e697a29763266660c80dbc1f2c478996`
  (código em `50ea980`; `5573717` e `48c0f8a` só documentação)
- Implementado por: GPT · Revisado por: Claude · Data: 2026-10-08
- Checagens (rodadas pelo Claude num worktree próprio no commit `48c0f8a`, Windows 11, Node 24):
  `npm ci` ok · `npm run typecheck` ok · `npm test` **355 passaram, 16 arquivos** (confere com o relatado).
  Rust não foi alterado na branch.
- Não verificado: comportamento no D1 remoto (a migração 0003 não foi aplicada lá; os testes usam SQLite real localmente).

## O que está correto

- O upsert de `updatePresence` é **uma única instrução** (`INSERT … ON CONFLICT DO UPDATE … WHERE … RETURNING`): sequência menor/igual com
  linha fresca não altera nada e devolve 409; linha expirada aceita qualquer sequência (reinício do app). `MAX(seen_at_ms, …)` impede voltar no tempo.
- O critério de frescor é o mesmo na gravação (`seen_at_ms < now - ttl` = expirado) e na leitura (`seen_at_ms >= now - ttl`), sem off-by-one.
- O isolamento entre grupos é feito no SQL: só membros do grupo consultado (`crew_members`), só quem está livre ou ativo **neste** grupo.
  Quem está ativo noutro grupo é omitido, então nem a identidade do outro grupo vaza. `active_crew` exige associação (403).
- Identidade só vem da assinatura; tempo só do Worker; validação de `game` coerente com o crate de sessão (≤ 64 bytes, sem caracteres de controle).

## Achados

### 1. [importante] O contrato não transporta `seated_since_ms`, que o crate de sessão passou a exigir
- Onde: `worker/SPEC.md` ("Presence contract"), `worker/src/validate.ts:parsePresence`, `migrations/0003_presence.sql`
- Problema: a revisão do `duoclip-session` (branch `claude/duoclip-session`, `2ec1b6d`) acrescentou `Presence.seated_since_ms: Option<u64>`
  para que todos os PCs concordem sobre quem ocupa os 8 lugares e para manter os lugares "grudados". **Não é falha do GPT**: o Worker foi
  feito às 11:50 e esse campo só foi commitado às 11:58. Mas, como está, o Worker descarta o campo, e a nota "the other announcement fields
  retain their names" deixa de ser verdade.
- Cenário que falha: 9 amigos do mesmo grupo no mesmo jogo. Sem `seated_since_ms`, todos os PCs ordenam só por `online_since_ms`; quando
  um amigo que entrou cedo reinicia o app e volta, ele "rouba" o lugar de alguém que estava na sessão, e o clipe seguinte não inclui quem saiu.
  O crate de sessão foi desenhado justamente para evitar isso.
- Sugestão: acrescentar `seated_since_ms: null | safe integer` ao `POST` (obrigatório, como os outros), à tabela (migração 0003 ainda não
  foi aplicada no D1 remoto, então dá para alterar a própria 0003), ao `GET` e aos testes.

### 2. [importante] Semântica de `online_since_ms` diverge entre o Worker e a sessão
- Onde: `worker/SPEC.md`: "`online_since_ms` is an opaque value from the device's monotonic clock"
- Problema: o SPEC do `duoclip-session` diz que `now_ms`, `online_since_ms` e `seated_since_ms` devem estar no **relógio global DuoClip**
  (AppClock, em ms), para serem comparáveis entre PCs (ordem justa da fila e lugares estáveis). Com o relógio monotônico de cada PC
  (contado desde o boot), um PC ligado há mais tempo "parece ter chegado antes" e fura a fila.
- Sugestão: no SPEC do Worker, trocar "device's monotonic clock" por "milissegundos do relógio global DuoClip (AppClock), opacos para o Worker".
  O Worker não precisa mudar código: continua sem usar esses valores para TTL ou autorização.

### 3. [importante] Custo em gravações do D1 pode estourar o plano grátis com 8 pessoas
- Onde: frequência sugerida no SPEC ("heartbeat every 10 s"), índice `idx_device_presence_seen_at`, e o anti-replay já existente
- Fato verificado: no plano grátis do D1, o limite é **100.000 linhas gravadas por dia**, e "Indexes will add an additional written row when
  writes include the indexed column" ([preços do D1](https://developers.cloudflare.com/d1/platform/pricing/)).
- Estimativa (é uma **estimativa minha**, baseada nessa regra; inclui a chave primária `TEXT` de `seen_signatures` como índice):
  - cada requisição autenticada grava 1 linha em `seen_signatures` + 2 índices (chave primária e `seen_at`) = 3, e a limpeza depois apaga as mesmas 3;
  - cada heartbeat grava mais 2 (linha de presença + índice de `seen_at_ms`).
  - Heartbeat e `GET` a cada 10 s ≈ 14 linhas por PC a cada 10 s ≈ 5.000 por hora. **Com 8 pessoas, ~40.000 por hora: o limite diário
    acaba em ~2,5 h de jogo.** Parte do custo é do anti-replay da Fase A (feita pelo Claude), mas a presença multiplica o número de requisições.
- Sugestões (decidir antes de integrar a sessão):
  - o `POST /v1/presence` devolver a presença dos grupos do chamador (evita o `GET` separado e corta ~40%);
  - heartbeat a cada 20–30 s com TTL de 60–90 s (o crate de sessão tem `presence_ttl_ms` configurável e período de graça);
  - remover `idx_device_presence_seen_at` (a tabela tem uma linha por dispositivo; a limpeza horária pode varrer sem índice);
  - registrar a conta no SPEC e um teste de "orçamento" simples.

### 4. [menor] Ordenação e campos do `GET` vs. o que a sessão consome
- Onde: `GET /v1/crews/:crew/presence`
- Problema: o cliente precisa aplicar cada resposta como um **snapshot completo** (o SPEC diz isso, muito bem); hoje o `SessionManager` só tem
  `on_presence` por dispositivo e expira pelo próprio TTL local. Não é bug do Worker, é trabalho do adaptador.
- Sugestão: deixar registrado como pendência da Fase C no quadro (adaptador Worker → `SessionManager` com remoção de quem saiu do snapshot).

## Veredito
**Mudanças necessárias** — o código está correto e bem testado; os achados 1 e 2 são ajustes pequenos de contrato que precisam entrar
antes do merge (ainda dá para alterar a migração 0003, que não foi aplicada no D1 remoto). O achado 3 precisa de uma decisão de frequência e
formato; pode ser resolvido nesta mesma branch ou virar tarefa própria antes de ligar a presença no app.

## Segunda rodada — conferência dos ajustes (2026-10-08)

- Commits conferidos: `9742e2058facd6c0deca5a9bb045014ec3742218` (código) e `deadb4c42fc2611ed0439b2a62e3ec8d4475635c` (HEAD),
  em resposta a `2026-10-08-claude-005-resultado-worker-presenca`. Resposta do autor: `worker/PRESENCA-AJUSTES.md`.
- Checagens refeitas pelo Claude: `npm run typecheck` ok · `npm test` **363 passaram, 16 arquivos**.
- Achado 1: **resolvido** — `seated_since_ms` (obrigatório, `null` ou inteiro seguro) no parser, na migração 0003, no upsert e nas respostas.
- Achado 2: **resolvido** — SPEC e README exigem o AppClock para `online_since_ms` e `seated_since_ms`. Melhoria além do pedido: o upsert
  ordena por (`online_since_ms`, `seq`), então uma execução nova do app é aceita na hora e um pacote atrasado da execução antiga é recusado.
- Achado 3: **resolvido** — o heartbeat devolve o retrato completo de todos os grupos do chamador numa só consulta (sem `GET` periódico),
  heartbeat de 30 s, TTL de 90 s e sem o índice de `seen_at_ms`. Refazendo a minha estimativa com a mesma regra da Cloudflare: ~7 linhas
  gravadas por heartbeat (1 da presença + 3 da assinatura + 3 da limpeza dela) → ~840 por hora por PC → com 8 PCs, ~15 h de jogo por dia
  antes do limite de 100.000 (antes eram ~2,5 h). Continua sendo estimativa.
- Achado 4: **resolvido** — o contrato do adaptador está documentado e virou a tarefa 17 (inclui `presence_ttl_ms = 90000` no `SessionConfig`;
  o padrão atual do crate de sessão é 30 s).

### Observação nova (menor, para a tarefa 17, não bloqueia)
- Com a ordem (`online_since_ms`, `seq`), se o app reiniciar antes de o AppClock sincronizar e anunciar um `online_since_ms` **menor** que o
  da execução anterior, os heartbeats serão recusados (409) por até 90 s. O adaptador deve garantir `online_since_ms` crescente entre
  execuções (por exemplo, `max(AppClock, último valor salvo + 1)`).

## Veredito (atualizado)
**Aprovado.** Pronto para o merge na branch de integração.
