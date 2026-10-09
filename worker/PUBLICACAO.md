# Publicar o backend DuoClip

O Worker permite que PCs diferentes usem grupos, convites e presença. Este roteiro usa
`duoclip-worker`, banco `duoclip` e bucket privado `povclip`, já configurados no
`wrangler.toml`. As chaves ficam em `worker/.dev.vars` da pasta principal, fora do Git.

## Preparação local

Na pasta `worker` da versão aprovada:

```powershell
npm.cmd run prepare:deploy
```

O comando executa typecheck, testes e `wrangler deploy --dry-run --outdir dist/deploy`.
Para na primeira falha e gera o pacote em `dist/deploy/`, ignorado pelo Git, sem publicar.
O dry-run permite inspecionar o código empacotado; não valida permissões ou recursos
remotos. Esse uso é documentado em [Bundling](https://developers.cloudflare.com/workers/wrangler/bundling/).

## Etapas remotas, após aprovação da publicação

Execute uma etapa por vez; se houver erro, pare antes de continuar. Use a mesma conta
Cloudflare que possui o banco e o bucket. Não crie outro banco ou bucket para este roteiro.

### 1. Login e migrações

Se o Wrangler pedir login, use `npx.cmd wrangler login`. Depois consulte as migrações:

```powershell
npx.cmd wrangler d1 migrations list duoclip --remote
```

Aplique as pendentes:

```powershell
npx.cmd wrangler d1 migrations apply duoclip --remote
```

São `0001_init.sql`, `0002_abuse_limits.sql` e `0003_presence.sql`. Elas criam o esquema
de cadastro/grupos/clipes, os contadores e a presença. Repita a consulta e confirme que
nenhuma está pendente. As opções estão na [referência D1](https://developers.cloudflare.com/d1/wrangler-commands/).

### 2. Segredos do Worker

`.dev.vars` é configuração local; sua existência não significa que os segredos estejam
no Worker. Cadastre os dois valores pelo prompt do Wrangler, sem colocá-los na linha
de comando ou na conversa. Use os valores do arquivo local:

```powershell
npx.cmd wrangler secret put R2_ACCESS_KEY_ID
```

```powershell
npx.cmd wrangler secret put R2_SECRET_ACCESS_KEY
```

Confira os **nomes**, que não mostram os valores:

```powershell
npx.cmd wrangler secret list
```

Esses comandos modificam o Worker remoto e fazem parte da publicação autorizada.
O fluxo de prompts e listagem está na [referência de segredos](https://developers.cloudflare.com/workers/wrangler/commands/workers/#secret-put).

### 3. Expiração do bucket

No painel R2, em `povclip → Settings → Object lifecycle rules`, configure no prefixo
`clips/`: expirar objetos após **3 dias** e abortar multipart incompleto após **1 dia**.
Essas regras já foram autorizadas pelo usuário para o bucket dedicado. O comando de
administração falhou com código 10042 na tentativa anterior; não presumir que passou.
Os detalhes estão em [README.md](README.md#6-regra-de-ciclo-de-vida-do-r2-rede-de-segurança).

### 4. Publicar e guardar o endereço

```powershell
npm.cmd run deploy
```

Guarde o endereço HTTPS `*.workers.dev` exibido. Esse será o endereço usado pelo cliente
de rede dos PCs; `localhost` só funciona para desenvolvimento na própria máquina.

### 5. Validar a publicação

Substitua o endereço abaixo pelo que o deploy informou:

```powershell
$duoclipWorkerUrl = 'https://duoclip-worker.SEU-SUBDOMINIO.workers.dev'
Invoke-RestMethod "$duoclipWorkerUrl/v1/health"
```

Esperado: `ok = true`. Depois teste cadastro, assinatura, grupos e presença:

```powershell
npm.cmd run test:crew -- --url $duoclipWorkerUrl
```

Esse diagnóstico **cria dois dispositivos de teste e um grupo no D1 remoto** e termina
com os dois dispositivos inativos. Os registros permanecem; não captura tela/áudio,
não envia clipes ao R2 e não mostra chaves, convites ou assinaturas. Execute-o uma vez
na validação autorizada da publicação. `ok: true` e `cleanup: "idle"` confirmam o fluxo
de grupos, mas não confirmam os segredos de URLs assinadas ou a troca de clipes pelo app.

O acesso S3 já foi testado pelo usuário, conforme [R2-VALIDACAO.md](R2-VALIDACAO.md).
A validação final de upload/download **pelas rotas publicadas** vem com o cliente de
rede e os clipes cifrados. Não declarar o compartilhamento entre amigos pronto apenas
porque `/v1/health` respondeu.
