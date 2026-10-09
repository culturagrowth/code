# Revisão do cliente de rede — tarefa 22

- Autor: Claude. Revisor: GPT. Data: 09/10/2026. Critério leve da decisão 19.
- Commit examinado: `bb8c1d85c613fe9b085b2f07ff7aaedec0b06f79`, incluindo implementação `e1f1bed` e lockfile separado.
- Base do diff: `dc414f2309d6176c17d77f4ef65d2792dc2518d6`.
- Branch de revisão: `gpt/revisao-net-client`; worktree próprio `worktrees/gpt-revisao-net-client`.
- Escopo: `crates/duoclip-net/`, Cargo.lock; confronto com autenticação, validação, SQL e rotas do Worker e adaptador de presença.

## Resultado

O fluxo de configurar dois PCs, criar grupo, convidar, entrar e consultar membros/grupos funciona. Não encontrei bloqueador para esta ferramenta de configuração nem divulgação da chave privada, assinatura ou URL assinada nos caminhos de saída examinados. A ressalva abaixo deve ser tratada quando a sessão em rede for ligada ao app; não pede nova rodada agora.

## Verificações

- `cargo fmt --all -- --check`, clippy workspace/all-targets com `-D warnings` e check Windows GNU: passaram.
- `cargo test --workspace`: **649 passaram, 0 falharam, 26 ignorados**. Não reproduzi as duas falhas intermitentes mencionadas pelo autor nesta execução; a causa delas permanece desconhecida.
- Assinatura: teste Rust do fixture passou; os dois casos do fixture também foram reconferidos diretamente com `canonicalString`, `sha256Hex` e `verifySignature` do Worker atual, compilado por TypeScript.
- Testes HTTP de mock: formatos das rotas, corpo assinado, timestamps crescentes, mapeamento 409, PUT com Content-Length exato, GET e redação de URLs/credenciais passaram.
- Integração HTTP adicional: **o teste ignorado `local_worker` passou** com as rotas/autenticação/SQL reais do Worker servidas em 127.0.0.1:8787 por um adaptador Node e o SQLite migrado do harness existente. Inclui assinatura errada → 401, não membro → 403, convite, entrada idempotente, presença/409 e registro de clipe.
- O executável `duoclip-amigos` também passou, contra esse mesmo servidor, por `configurar` (duas pessoas), reconfigurar preservando a chave, `criar-grupo`, `convidar`, `entrar`, `grupos` e `status`. Os arquivos de identidade eram sintéticos e separados; nenhuma chave apareceu nas saídas coletadas.
- TLS: conferido o caminho `NativeTls`/`PlatformVerifier` no código instalado de ureq 3.4.2, com validação de certificados mantida e sem redirects. Testes de rede desta revisão foram somente HTTP loopback, não uma conexão HTTPS real.

### Limite do runtime local

As três migrações D1 locais passaram no worktree. `wrangler dev --local` falhou no build por `Cannot read directory ../../../../..: Access is denied`, antes de servir o Worker. Não alterei permissões ou proteção. O adaptador Node usa o harness já existente, as rotas reais e o SQL real; **não substitui a verificação do runtime Wrangler/Cloudflare**, que o autor relatou ter feito. Não foram usados `.dev.vars`, credenciais reais ou servidor/R2 remoto; nenhuma captura ou instalação.

Evidências locais, ignoradas pelo Git, em `test-output/review-net/`: `fmt.log`, `clippy.log`, `rust-tests.log`, `gnu.log`, `fixture-result.json`, `http-integration.log`, `result.json`, `migrations.log`, `worker-local.log` e roteiro `run-http-review.mjs`. O servidor temporário foi encerrado após o teste.

## Achado para depois

### NET-1 — menor nesta etapa: listar grupos pode substituir a presença da partida

- Arquivo: `crates/duoclip-net/src/cli.rs:413`, principalmente linha 422; interação com `worker/src/db.ts:305` e `crates/duoclip-presence/src/lib.rs:373`.
- Cenário reproduzido: o mesmo dispositivo anuncia jogo `cs2` e grupo ativo numa execução. Depois, `duoclip-amigos grupos` envia um heartbeat ocioso com `online_since_ms` mais novo. O Worker aceita, troca jogo/grupo por null e considera o anúncio seguinte da execução anterior obsoleto (409), mesmo com seq maior. O adaptador de presença mantém o run id ao receber 409; até expirar a presença ou reiniciar a execução, o anúncio antigo não recupera a sessão.
- Impacto: ao coexistirem o app com presença contínua e a ferramenta CLI, consultar grupos poderá tirar temporariamente este PC da sessão. **A presença contínua ainda não está ligada ao gravador/interface nesta entrega**, portanto não bloqueia o fluxo de preparação que foi solicitado.
- Sugestão para a integração: consultar grupos pelo cliente de presença já ativo, ou oferecer consulta sem heartbeat que não altere jogo/grupo/run id. Atualizar a nota do SPEC, que menciona só a possibilidade de o próprio CLI receber 409; o sentido contrário foi comprovado.
- Evidência: último check de `result.json`; o roteiro confirma jogo/grupo null no SQLite e resposta 409 para o heartbeat seguinte da execução anterior.

## Limites intencionais

Chave em arquivo por usuário sem DPAPI, nomes de grupos guardados localmente, chamadas bloqueantes e cliente de transferência de bytes seguem os desvios documentados. O chamador futuro deve fornecer bytes cifrados com `duoclip-crypto`. Esta revisão não valida a troca completa de MP4 cifrado entre dois PCs, sincronização de relógios ou upload/download no R2 real.

## Veredito

**Aprovado com ressalva menor (NET-1 para a ligação futura da presença).** Pronto para integração do cliente/ferramenta; nenhuma correção imediata solicitada, conforme a prioridade leve do usuário.
