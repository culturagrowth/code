# Revisão — tarefa nº 13: configuração Cloudflare (R2/D1) e teste no R2 real

- Branch: `gpt/worker-r2` · commits revisados: `a803b09..fddcf272fa7acfa8bb63191cec6a644ace6256e0`
  (código em `4b411c6`; os demais são documentação)
- Implementado por: GPT · Revisado por: Claude · Data: 2026-10-08
- Checagens (rodadas pelo Claude num worktree próprio no commit `fddcf27`, Windows 11, Node 24):
  `npm ci` ok · `npm run typecheck` ok · `npm test` **321 passaram, 16 arquivos** (confere com o relatado). Rust não foi alterado.
- Não verificado pelo Claude: o `npm run test:r2` contra o R2 real (usa as credenciais do `.dev.vars`; não executei para não
  gravar no bucket sem um pedido do usuário). Considero a evidência do usuário (`404, 200, 200, 403, 204, 404`, `cleanup: done`).
  Não li nem copiei o conteúdo do `.dev.vars`; só confirmei que `worker/.gitignore` o ignora.

## O que está correto

- O teste usa o **presigner real do Worker**, uma chave nova e aleatória no formato `clips/{crew}/{clip}/{pov}/proxy/000000.bin`, conteúdo
  cifrado (AES-GCM com chave descartável) e:
  - confirma `NoSuchKey` antes de escrever (não confunde bucket inexistente com objeto ausente);
  - confere o download byte a byte;
  - prova que o R2 **rejeita um tamanho diferente do assinado** (`SignatureDoesNotMatch`), o que valida a decisão da Fase A de assinar o `Content-Length`;
  - limpa mesmo se a resposta do PUT se perder, e só mostra a chave se a limpeza falhar.
- A saída é sanitizada: códigos de erro numa lista fechada, sem URLs assinadas, cabeçalhos, corpos de erro ou credenciais. Os 9 testes offline
  cobrem esses caminhos, inclusive com segredos-sentinela.
- `redirect: "error"` e timeout de 15 s em cada requisição. `dist/` ignorado pelo Git.

## Achados

### 1. [importante] O README e o SPEC ainda mandam usar `duoclip-clips`, mas a configuração aponta para `povclip`
- Onde: `worker/wrangler.toml` (`BUCKET_NAME` e `bucket_name` = `povclip`) × `worker/README.md` (seções 1, 2 e 6: criar `duoclip-clips`,
  restringir o token a `duoclip-clips`, regra de ciclo de vida em `duoclip-clips`)
- Contexto: **usar o `povclip` foi decisão do usuário** (o bucket já existia na conta). Não é um erro de configuração.
- Problema: quem seguir o README (inclusive a outra IA, no deploy) vai criar um segundo bucket ou aplicar a regra no bucket errado.
- Sugestão: atualizar README/SPEC para "bucket `povclip` (decisão do usuário em 08/10/2026)", com os comandos de ciclo de vida apontando para ele.

### 2. [importante] A regra de ciclo de vida do DuoClip apaga **tudo** em `clips/` do bucket compartilhado
- Onde: `worker/README.md`, seção 6 (`expire-clips clips/ --expire-days 3`)
- Problema: o `povclip` foi criado antes, para outro uso. Se ele ainda guarda objetos de outro app sob o prefixo `clips/`, a regra os apaga
  após 3 dias. Não verifiquei o conteúdo do bucket (é uma conta do usuário; listar exige credenciais).
- Cenário que falha: um arquivo de outro app em `clips/abc.mp4` → aplicar a regra → o arquivo some em até ~3 dias.
- Sugestão: antes de aplicar a regra, o usuário confirma se o bucket tem outros dados. Se tiver, usar um prefixo exclusivo do DuoClip
  (por exemplo `duoclip/clips/…`, o que muda `object_key` no proto e no Worker) ou um bucket só do DuoClip.

### 3. [menor] `ACCOUNT_ID` versionado no `wrangler.toml`
- Onde: `worker/wrangler.toml`
- Problema: o Account ID não é segredo (o próprio comentário do arquivo diz isso, e ele aparece nas URLs assinadas), mas fica público se o
  repositório for público. Não sei se `nicolaspercio1/duoclip` é público.
- Sugestão: manter, se o repositório for privado; caso contrário, considerar `wrangler.toml` com placeholder e o valor em `.dev.vars`/variável do deploy.

### 4. [menor] O `test:r2` depende do Node com `util.parseEnv`
- Onde: `worker/tools/r2-smoke.ts` (`import { parseEnv } from "node:util"`)
- Problema: suposição minha, não verificada: `parseEnv` não existe em versões antigas do Node. No PC do usuário (Node 24) funciona.
- Sugestão: declarar `"engines": { "node": ">=22" }` no `package.json` ou citar a versão no `R2-VALIDACAO.md`.

## Veredito
**Aprovado com ressalvas** — o código e o teste estão corretos e bem protegidos. Os achados 1 e 2 são de documentação e operação e precisam
estar resolvidos **antes de aplicar a regra de ciclo de vida e antes do deploy**; não bloqueiam o merge do código, mas peço que o achado 1
entre nesta branch antes do merge, para o README não contradizer a configuração.

## Segunda rodada — conferência dos ajustes (2026-10-08)

- Commit conferido: `623475869038aaff55eb31f7fc352682bec66361` (`gpt/worker-r2`), em resposta a
  `2026-10-08-claude-006-resultado-worker-r2` e `2026-10-08-claude-009-decisao-bucket`.
- Checagens refeitas pelo Claude: `npm run typecheck` ok · `npm test` **321 passaram**.
- Achado 1: **resolvido** — README, SPEC e `.dev.vars.example` usam `povclip`; não há mais `duoclip-clips` em `worker/`
  (`grep` em `.md`, `.toml` e `.example`). A decisão do usuário está registrada no README.
- Achado 2: **resolvido pela decisão do usuário** — bucket exclusivo do DuoClip; a regra em `clips/` vale. A aplicação pelo painel
  está documentada, com o erro 10042 da API registrado sem presumir a causa.
- Achado 3: aceito como está (identificador não secreto).
- Achado 4: **resolvido** — `package.json` declara `"node": ">=22.13"`.

## Veredito (atualizado)
**Aprovado.** Pronto para o merge na branch de integração.
