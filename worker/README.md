# DuoClip Worker

Backend mínimo do DuoClip, em um **Cloudflare Worker** (TypeScript) com **R2** (clipes cifrados) e **D1** (cadastro).

O Worker **nunca vê o conteúdo dos clipes**: os apps cifram tudo de ponta a ponta (`duoclip-crypto`) e só
enviam/baixam bytes cifrados direto do R2, por **URLs pré-assinadas de 15 minutos**. O Worker apenas:

- autentica cada instalação por **assinatura Ed25519** (a chave privada fica no PC, protegida por DPAPI);
- gerencia **crews** (grupos de amigos), convites e membros;
- registra os clipes e aplica **cotas** (por clipe, por dia e por requisição) e **limites contra abuso**;
- gera as URLs pré-assinadas de upload (PUT) e download (GET);
- apaga os clipes expirados (rota `DELETE` e **varredura de hora em hora**).

Contexto e decisões: `docs/pesquisa-app-clipes-sincronizados.md`, seção 10.

```
App (PC)  ── HTTPS + assinatura Ed25519 ──▶  Worker  ──▶  D1 (devices, crews, clips, cotas)
   │                                           │
   │                                           └──▶  R2 (binding CLIPS: list/delete) e SigV4 (URLs pré-assinadas)
   └── PUT/GET de blocos cifrados direto no R2, usando as URLs pré-assinadas (válidas por 15 min)
```

## Requisitos

- Node.js 22.13 ou mais novo (os testes usam o SQLite embutido do Node e o WebCrypto com Ed25519).
- Uma conta Cloudflare com **R2** habilitado (o plano gratuito basta para um grupo de amigos).
- `npm install` dentro de `worker/`. O `wrangler` já vem como dependência de desenvolvimento, então use `npx wrangler ...`.

```sh
cd worker
npm install
npx wrangler login
```

## Configuração passo a passo

### 1. Criar o bucket R2

```sh
npx wrangler r2 bucket create duoclip-clips
```

**Mantenha o bucket privado:** não habilite o acesso público (domínio `r2.dev` ou domínio personalizado) nem CORS aberto.
Todo acesso aos objetos deve ser feito por URLs pré-assinadas.

Opcional: acrescente `--location enam` (ou outra dica de localização) para ficar mais perto dos jogadores. O R2 não
tem região na América do Sul.

Se usar outro nome de bucket, altere `bucket_name` em `[[r2_buckets]]` **e** `BUCKET_NAME` em `[vars]` no `wrangler.toml`
(os dois precisam ser iguais).

### 2. Criar o token de API do R2 (acesso S3)

As URLs pré-assinadas usam a API S3 do R2, que exige uma chave de acesso. O binding `CLIPS` do Worker não precisa
dela, só a assinatura das URLs.

1. No painel da Cloudflare, abra **R2 → Overview → Manage API tokens** (Gerenciar tokens de API) e clique em
   **Create API token**.
2. Permissão: **Object Read & Write**.
3. Em **Specify bucket(s)**, escolha somente `duoclip-clips`. Não use um token de conta inteira.
4. Crie o token e copie, **uma única vez** (o segredo não é mostrado de novo):
   - **Access Key ID** → será o segredo `R2_ACCESS_KEY_ID`;
   - **Secret Access Key** → será o segredo `R2_SECRET_ACCESS_KEY`.
5. Anote também o **Account ID** (32 caracteres hexadecimais, visível na página inicial do R2 ou na barra lateral do painel).
   Ele vira a variável `ACCOUNT_ID`.

### 3. Criar o banco D1 e aplicar as migrações

```sh
npx wrangler d1 create duoclip
```

O comando imprime um `database_id`. Cole esse valor em `database_id` na seção `[[d1_databases]]` do `wrangler.toml`
e depois aplique o esquema:

```sh
npx wrangler d1 migrations apply duoclip --remote
```

As migrações ficam em `migrations/` e são aplicadas em ordem:

- `0001_init.sql` cria `devices`, `crews`, `crew_members`, `invites`, `clips`, `clip_chunks`, `usage` e `seen_signatures`;
- `0002_abuse_limits.sql` cria `counters` (contadores diários atômicos dos limites contra abuso) e dois índices.

Rode o mesmo comando depois de qualquer migração nova (inclusive ao atualizar o Worker).

### 4. Variáveis e segredos

Variáveis **não secretas** ficam no `wrangler.toml`, em `[vars]`:

```toml
[vars]
ACCOUNT_ID = "0123456789abcdef0123456789abcdef"   # o seu Account ID
BUCKET_NAME = "duoclip-clips"
```

Os dois valores da chave S3 são **segredos** e nunca entram no repositório:

```sh
npx wrangler secret put R2_ACCESS_KEY_ID
npx wrangler secret put R2_SECRET_ACCESS_KEY
```

Duas variáveis **opcionais** ajustam os disjuntores contra abuso (veja "Limites e cotas"). Sem elas valem os padrões;
um valor ausente ou malformado (qualquer coisa que não seja um inteiro não negativo) também cai no padrão:

```toml
# MAX_NEW_DEVICES_PER_DAY = "50"             # cadastros novos por dia UTC, no serviço todo ("0" fecha o cadastro)
# MAX_GLOBAL_DAILY_BYTES = "214748364800"    # 200 GiB anunciados para upload por dia UTC, no serviço todo
```

**Dica:** o cadastro de dispositivos (`POST /v1/devices`) é aberto por projeto. Depois que todos os seus amigos
estiverem cadastrados, coloque `MAX_NEW_DEVICES_PER_DAY = "0"` e faça o deploy: ninguém mais consegue criar
dispositivos (os já cadastrados continuam funcionando). Reabra quando precisar de alguém novo.

Se `ACCOUNT_ID`, `BUCKET_NAME` ou algum segredo estiver ausente ou inválido (por exemplo, o texto `REPLACE_WITH_...` do
modelo), as rotas que geram URLs respondem `500 internal_error` e o log do Worker diz qual variável está errada
(sem mostrar valores). As demais rotas continuam funcionando.

### 5. Publicar

```sh
npm run typecheck && npm test      # opcional, mas recomendado
npm run deploy                     # wrangler deploy
```

Teste a publicação (o endereço aparece no final do `deploy`):

```sh
curl https://duoclip-worker.<seu-subdominio>.workers.dev/v1/health
# {"ok":true}
```

O gatilho de cron `0 * * * *` (hora cheia, UTC) é criado junto com o deploy. Os logs da varredura (uma linha JSON por
execução, com as contagens) aparecem em **Workers & Pages → duoclip-worker → Logs**, ou em tempo real com
`npx wrangler tail`.

### 6. Regra de ciclo de vida do R2 (rede de segurança)

A expiração de verdade é feita pelo Worker (exclusão explícita e varredura de hora em hora). Mesmo assim, crie uma regra
de ciclo de vida no bucket como **rede de segurança**, para o caso de o Worker ficar parado, de um upload ter chegado
depois da exclusão ou de um multipart ter ficado pela metade:

- prefixo `clips/`: **expirar objetos após 3 dias**;
- prefixo `clips/`: **abortar uploads multipart incompletos após 1 dia**.

Pelo painel: **R2 → duoclip-clips → Settings → Object lifecycle rules → Add rule**. Ou pela linha de comando:

```sh
npx wrangler r2 bucket lifecycle add duoclip-clips expire-clips clips/ --expire-days 3
npx wrangler r2 bucket lifecycle add duoclip-clips abort-multipart clips/ --abort-multipart-days 1
npx wrangler r2 bucket lifecycle list duoclip-clips
```

O ciclo de vida do R2 trabalha em dias e roda de forma assíncrona (a remoção pode demorar até cerca de 24 h depois de
vencer). Por isso ele **não** substitui a varredura horária: é só o limite máximo caso tudo mais falhe.

## Desenvolvimento local

```sh
cp .dev.vars.example .dev.vars          # edite os valores; o arquivo é ignorado pelo git
npx wrangler d1 migrations apply duoclip --local
npm run dev                             # wrangler dev, com D1 e R2 simulados localmente
curl http://localhost:8787/v1/health
curl "http://localhost:8787/cdn-cgi/local/scheduled"    # dispara a varredura manualmente
```

Localmente o D1 e o R2 são simulados, mas as URLs pré-assinadas apontam para o R2 **real** (a assinatura usa a sua chave).
Para testar upload e download de verdade, use um bucket de teste.

## Testes

```sh
npm run typecheck    # tsc --noEmit (strict)
npm test             # vitest run, em ambiente Node
```

Os testes não usam rede nem login do wrangler e rodam em poucos segundos:

- o SQL de `src/db.ts` roda de verdade num SQLite em memória (`node:sqlite`) carregado com todas as migrações de
  `migrations/` (o D1 é SQLite), então as cotas atômicas, o consumo de convites e a proteção contra replay são testados com o SQL real;
- o R2 é substituído por um armazenamento em memória; o relógio e o sorteio são controlados e determinísticos;
- as chaves de objeto são comparadas com *golden strings* idênticas às dos testes Rust de `crates/duoclip-proto`;
- as URLs pré-assinadas são conferidas contra uma implementação independente do SigV4.

## Estrutura

| Arquivo | Função |
|---|---|
| `src/index.ts` | `fetch` e `scheduled`; liga os bindings reais (D1, R2, segredos) |
| `src/routes.ts` | Roteador e handlers das rotas |
| `src/auth.ts` | String canônica, verificação Ed25519, janela de tempo, anti-replay |
| `src/keys.ts` | Nomes e validação das chaves de objeto (idêntico ao `duoclip-proto`) |
| `src/presign.ts` | URLs pré-assinadas (`aws4fetch`) |
| `src/db.ts` | Consultas D1 tipadas, atrás da interface `Db` |
| `src/quota.ts` | Constantes e contas de cota |
| `src/validate.ts` | Validação dos corpos das requisições |
| `src/sweep.ts` | Varredura horária |
| `src/store.ts`, `src/invite.ts`, `src/http.ts`, `src/encoding.ts`, `src/app.ts`, `src/env.ts` | Apoio |
| `migrations/0001_init.sql`, `migrations/0002_abuse_limits.sql` | Esquema do D1 |

## Referência da API

Tudo é JSON, com corpo de no máximo **16 KiB** (`413` acima disso). Todos os UUIDs são **minúsculos e com hífens**.
Erros têm o formato `{"error": "<codigo>", "message": "<texto>"}`. Os tempos (`expires_at` etc.) são
milissegundos Unix.

### Autenticação

`POST /v1/devices` e `GET /v1/health` são as únicas rotas sem assinatura. As demais exigem:

| Cabeçalho | Valor |
|---|---|
| `X-DC-Device` | `device_id` (UUID minúsculo) |
| `X-DC-Timestamp` | milissegundos Unix, inteiro decimal |
| `X-DC-Signature` | Base64 padrão da assinatura Ed25519 de 64 bytes sobre a string canônica |

String canônica (separada por `\n`, sem `\n` no final). O caminho assinado é o `pathname + search` da URL **já
serializada** (WHATWG), isto é, como o Worker a enxerga: na query, espaços viram `%20`, apóstrofos viram `%27` etc.
Nenhuma rota usa query string, então o mais simples é não enviar nenhuma.

```
DC1
{METODO em maiúsculas}
{CAMINHO com query, exatamente como enviado}
{X-DC-Timestamp}
{sha256 hexadecimal minúsculo do corpo bruto; do corpo vazio se não houver}
```

Regras:

- diferença de relógio acima de **±5 minutos** → `401` com `reason: "clock_skew"` e o campo `server_time_ms`
  (o app pode usá-lo para se corrigir);
- a mesma assinatura repetida em até **10 minutos** → `401` com `reason: "replay"`;
- falha de autenticação → sempre `401 unauthorized`, com `reason` em `missing_headers`, `bad_headers`, `clock_skew`,
  `unknown_device`, `bad_signature` ou `replay`;
- a metade `S` da assinatura precisa ser menor que a ordem do grupo (assinatura maleável `S + L` é recusada com
  `bad_signature`, para que um mesmo pedido não tenha duas assinaturas válidas e escape do anti-replay);
- não é membro da crew → `403 not_a_member`.

### Rotas

| Rota | Quem | Corpo | Resposta |
|---|---|---|---|
| `GET /v1/health` | público | | `{"ok":true}` |
| `POST /v1/devices` | público | `{device_id, public_key_b64, display_name}` | `201` (novo) ou `200` (mesma chave) com `{device_id}`; `409 device_exists` se o id existe com outra chave; `429 registration_limited` quando o limite diário de cadastros novos acabou |
| `POST /v1/crews` | qualquer dispositivo | `{name}` (1 a 48 caracteres) | `201 {crew_id}`; o criador já é membro; `409 crew_limit` depois de 20 crews criadas |
| `POST /v1/crews/:crew/invites` | membro | | `201 {code, expires_at}`; código de 10 caracteres `[A-Z2-9]`, válido por 24 h, 5 usos; `409 invite_limit` com 10 convites válidos ao mesmo tempo |
| `POST /v1/crews/join` | qualquer dispositivo | `{code}` | `200 {crew_id}`; `404 invalid_invite` se for inválido, vencido ou sem usos; `429 too_many_attempts` depois de 20 tentativas falhas no dia |
| `GET /v1/crews/:crew/members` | membro | | `[{device_id, display_name}]` |
| `POST /v1/clips` | membro da crew | `{clip_id, crew_id, ttl_s?}` | `201` ou `200` (idempotente para o mesmo dono) com `{clip_id, expires_at}`; `429 clip_registration_limited` depois de 200 clipes novos no dia |
| `POST /v1/clips/:clip/upload-urls` | membro, e `pov` = quem chama | `{pov, quality, indices[], sizes[], manifest?, manifest_size?}` | URLs PUT, veja abaixo |
| `POST /v1/clips/:clip/download-urls` | membro | `{pov, quality, indices[], manifest?}` | URLs GET, veja abaixo |
| `DELETE /v1/clips/:clip` | dono ou qualquer membro | | `{ok:true, deleted_objects, complete}`; pode ser repetido (apaga também o que chegou depois) |

Detalhes:

- `public_key_b64`: Base64 padrão (com `=`) dos **32 bytes crus** da chave pública Ed25519. `display_name`: 1 a 32 caracteres.
- Convites: tentar entrar de novo com o mesmo dispositivo devolve `200` sem gastar outro uso. O código é aceito em
  minúsculas.
- `ttl_s`: 1 a 259200 (72 h), padrão 259200. Registrar de novo o mesmo `clip_id` pelo mesmo dono devolve o registro
  original, sem alterar a validade. Outro dono ou outra crew recebe `409 clip_exists`.
- `quality`: `"proxy"` ou `"full"`. `manifest`: booleano (padrão `false`); pede também a URL do `manifest.bin`.
- `manifest_size` (só no upload): **obrigatório** quando `manifest` é `true`; é o tamanho exato, em bytes, do
  `manifest.bin` cifrado (1 a 1 MiB, senão `413 manifest_too_large`). Ele é cobrado nas cotas como se fosse um bloco
  e entra na assinatura da URL, como o tamanho de cada bloco.
- `DELETE`: apaga todos os objetos do prefixo do clipe e marca o clipe como apagado. `complete: false` significa que
  o limite de chamadas ao R2 de uma requisição foi atingido antes de esvaziar o prefixo (não acontece com clipes dentro
  das cotas); basta repetir o `DELETE`.
- Clipe inexistente → `404 clip_not_found`; apagado → `410 clip_gone`; vencido → `410 clip_expired`.

#### Resposta de `upload-urls` e `download-urls`

```json
{
  "expires_in": 900,
  "expires_at": 1791426963900,
  "headers": { "Content-Type": "application/octet-stream" },
  "chunks": [
    { "index": 0, "key": "clips/{crew}/{clip}/{pov}/full/000000.bin", "content_length": 4194304, "url": "https://<ACCOUNT_ID>.r2.cloudflarestorage.com/<BUCKET>/clips/...?X-Amz-Expires=900&..." }
  ],
  "manifest": { "key": "clips/{crew}/{clip}/{pov}/full/manifest.bin", "content_length": 812, "url": "https://..." }
}
```

- `headers` e `content_length` (só no upload): o `PUT` **deve** enviar exatamente `Content-Type: application/octet-stream`
  e exatamente `Content-Length: <content_length>` (o tamanho anunciado em `sizes[]` / `manifest_size`). Os dois
  cabeçalhos fazem parte da assinatura (`X-Amz-SignedHeaders=content-length;content-type;host`): com outro valor o R2
  recusa com `403 SignatureDoesNotMatch`. Assim ninguém consegue guardar mais bytes do que as cotas cobraram. Um
  cliente HTTP normal já define o `Content-Length` sozinho quando o corpo tem tamanho conhecido; não use
  `Transfer-Encoding: chunked`.
- `manifest` é `null` quando não foi pedido.
- As URLs valem **15 minutos** (`X-Amz-Expires=900`). Para retomar, peça novas URLs; pedir de novo o mesmo bloco com o
  mesmo tamanho **não** gasta cota.
- `download-urls` não confere se o objeto já existe: se o outro jogador ainda não terminou o upload, o GET volta `404`
  do R2 e o app tenta de novo.

#### Formato das chaves de objeto

Igual ao de `object_key`, `manifest_key` e `clip_prefix` em `crates/duoclip-proto`:

```
clips/{crew}/{clip}/{pov}/{proxy|full}/{index:06}.bin
clips/{crew}/{clip}/{pov}/{proxy|full}/manifest.bin
clips/{crew}/{clip}/                                  (prefixo apagado pelo DELETE e pela varredura)
```

Exemplo (UUIDs `00000000-...-0001`, `-0002`, `-0003`):
`clips/00000000-0000-0000-0000-000000000001/00000000-0000-0000-0000-000000000002/00000000-0000-0000-0000-000000000003/proxy/000007.bin`

### Limites e cotas

| Limite | Valor | Resposta ao exceder |
|---|---|---|
| Corpo da requisição | 16 KiB | `413 payload_too_large` |
| Índices por requisição | 64 (sem repetidos; índice de 0 a 999999) | `400 too_many_indices` |
| Tamanho de um bloco (cifrado) | 64 MiB | `413 chunk_too_large` |
| Total por clipe (todos os POVs e qualidades) | 1,5 GiB | `413 clip_quota_exceeded` |
| Total por dispositivo por dia UTC | 10 GiB | `429 daily_quota_exceeded`, com `Retry-After` até a meia-noite UTC |
| Vida de um clipe | 72 h | |
| `manifest.bin` (cifrado) | 1 MiB, tamanho exato informado em `manifest_size` | `413 manifest_too_large` |
| Cobrança mínima por objeto | 256 KiB (um clipe guarda no máximo 6144 objetos) | conta como 256 KiB nas cotas |
| Cadastros novos de dispositivos por dia UTC (serviço todo) | 50 (`MAX_NEW_DEVICES_PER_DAY`) | `429 registration_limited` |
| Bytes anunciados por dia UTC (serviço todo) | 200 GiB (`MAX_GLOBAL_DAILY_BYTES`) | `429 global_quota_exceeded` |
| Crews criadas por dispositivo | 20 | `409 crew_limit` |
| Convites válidos ao mesmo tempo por crew | 10 | `409 invite_limit` |
| Tentativas de entrar com convite por dispositivo por dia UTC (só as que falham contam) | 20 | `429 too_many_attempts` |
| Clipes novos por dispositivo por dia UTC | 200 | `429 clip_registration_limited` |

Os limites diários de `429` trazem `Retry-After` até a meia-noite UTC.

As cotas valem para os **tamanhos informados** em `sizes[]` e `manifest_size`, e esses tamanhos são **assinados** nas URLs
(`Content-Length`), então valem de verdade. Cada objeto é cobrado pelo maior entre o seu tamanho e 256 KiB, para que
milhões de objetos minúsculos não custem quase nada em cota e muito em operações do R2. As reservas são atômicas no D1
(sem corrida entre requisições simultâneas), e uma requisição recusada não deixa nada reservado. Pedir de novo a URL do
mesmo objeto com o mesmo tamanho (ou menor) não custa nada.

## Varredura horária (`scheduled`)

Todo início de hora o Worker:

1. apaga os objetos R2 de cada clipe com `expires_at < agora` (listando e apagando o prefixo em lotes de até 1000);
2. apaga a linha do clipe, mas só quando o vencimento foi há mais de 15 minutos **e** o prefixo foi esvaziado por
   completo. Assim a execução seguinte ainda consegue remover um upload tardio que tenha usado uma URL ainda válida, e
   nunca se perde a referência de um clipe que ainda tem objetos;
3. apaga assinaturas anti-replay com mais de 15 minutos, convites vencidos ou sem usos e contadores de uso e de limites
   com mais de 7 dias;
4. registra uma linha JSON com as contagens (`clips_processed`, `objects_deleted`, `clip_rows_deleted`, `clip_failures`,
   `clips_incomplete`, `clips_deferred`, `signatures_purged`, `invites_purged`, `usage_rows_purged`,
   `counter_rows_purged`).

Um clipe que falha (erro do R2) é registrado e tentado de novo na próxima hora, sem impedir os outros. O plano gratuito
limita cada invocação a 50 subrequisições (chamadas ao D1 e ao R2 contam), então cada execução faz no máximo 40 chamadas
ao R2 (`SWEEP_STORE_CALL_BUDGET`) e olha no máximo 20 clipes. O que não coube (`clips_deferred`, ou um clipe grande
esvaziado só em parte, `clips_incomplete`) fica com a linha intacta para a próxima hora. No plano pago esses números
podem ser aumentados em `src/sweep.ts`.

## Segurança e limitações conhecidas

- **Conteúdo:** o Worker só vê nomes de objetos e tamanhos informados. Os blocos e o `manifest.bin` chegam cifrados dos
  apps; a chave do clipe nunca passa pelo Worker.
- **Cotas por tamanho assinado:** `Content-Type` e `Content-Length` entram na assinatura das URLs de upload, então cada
  objeto tem exatamente o tamanho cobrado, inclusive o `manifest.bin`. Uma URL de upload sem tamanho nunca é emitida.
  Isso foi conferido contra uma implementação independente do SigV4 nos testes, mas **não** contra o R2 real (não há
  credenciais nos testes). Se o R2 recusar o `Content-Length` assinado, o sintoma é `403 SignatureDoesNotMatch` em todo
  upload; nesse caso a regra de ciclo de vida de 3 dias continua limitando o dano, e a assinatura do tamanho pode ser
  retirada em `src/presign.ts` (voltando ao risco de um membro enviar mais bytes do que anunciou).
- **Cadastro aberto e disjuntores:** `POST /v1/devices` é aberto por projeto, então qualquer pessoa que descubra o
  endereço poderia criar dispositivos e, com eles, cota própria. Os disjuntores (cadastros por dia, bytes por dia no
  serviço todo, crews, convites, clipes) limitam o custo no pior caso. O outro lado da moeda: alguém que consiga
  esgotar esses limites derruba novos cadastros ou os uploads do dia (`429`), sem acesso a nenhum clipe. Depois de
  cadastrar os amigos, use `MAX_NEW_DEVICES_PER_DAY = "0"`.
- **Limite de taxa:** além disso, crie regras de **Rate limiting** da Cloudflare (Security → WAF → Rate limiting rules)
  para `POST /v1/devices` e `POST /v1/crews/join`, e um limite geral por IP para `/v1/*`: cada requisição assinada grava
  uma linha no D1 (anti-replay). O código do convite tem 34^10 combinações, 5 usos e só 20 tentativas falhas por
  dispositivo por dia, então adivinhar é inviável.
- **Upload tardio:** uma URL já emitida continua válida por até 15 minutos mesmo depois de o clipe ser apagado. A
  varredura (segunda passada), um novo `DELETE` e a regra de ciclo de vida removem o que sobrar.
- **Existência de ids:** um membro de outra crew recebe `403` e um id inexistente recebe `404`; como os ids são UUIDs
  aleatórios, isso não permite enumerar nada na prática.
- **Relógio:** a assinatura vale ±5 minutos. O app deve sincronizar o relógio (ou usar `server_time_ms` do erro
  `clock_skew`).
- **Dependências de desenvolvimento:** o `npm audit` aponta avisos na cadeia `wrangler → miniflare → sharp`. Eles afetam
  só o ambiente de desenvolvimento, não o Worker publicado.
