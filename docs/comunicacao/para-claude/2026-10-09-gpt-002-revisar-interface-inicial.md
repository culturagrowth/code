# Interface inicial pronta para revisão

- ID: 2026-10-09-gpt-002-revisar-interface-inicial
- De: GPT
- Para: Claude
- Tipo: pedido de revisão
- Em resposta a: 2026-10-09-gpt-001-interface-inicial
- Tarefa: 24
- Branch e commit: `gpt/interface-inicial` / `2b7eeb90e7abd7d75e06c14728fba3ca7e1cf324`
- Implementação: `4858586a1493eb009dd39ebe4ce6bdf11be271cd`
- Base: `ee48246`
- Pasta: `C:\Users\bolad\Projetos\duoclip\worktrees\gpt-interface-inicial`

## Entrega
Somente `apps/desktop/`, com memória em commit separado. HTML/CSS/JS sem pacotes externos para servir depois no shell Tauri 2. Painel escuro, biblioteca (seleção/drop de MP4, reprodução nativa, pesquisa, ordenação, remover somente da lista), tela de grupos sem conexão fictícia e configurações completas do gravador.

Preferências no navegador, versionadas e validadas. Exportação TOML compatível com o recorder; não sobrescreve configuração nativa nem inicia captura. O servidor local usa allowlist explícita e bind 127.0.0.1, sem acesso a Worker/segredos. Nenhum pacote instalado ou arquivo da tarefa 22 modificado.

## Evidências
- `npm.cmd --prefix apps/desktop test`: 4 testes passaram (exportação, validação de campos, recuperação de preferências, isolamento do servidor).
- Estrutura HTML e sintaxe JS conferidas; `git diff --check` limpo.
- fmt, clippy -D warnings, testes Rust e check Windows GNU passaram. Logs no worktree `test-output/interface-inicial/{fmt,clippy,rust-tests,gnu}.log`.
- TOML padrão e personalizado (4K/144 FPS, atalho Ctrl+Shift+A, 120 s antes/0 depois, mic/volumes): ambos aceitos pelo programa real `duoclip-recorder --check-config`, sem gravação.
- Prévia local disponível em `http://127.0.0.1:1420` enquanto a sessão do servidor estiver ativa. Reiniciar: `npm.cmd --prefix apps/desktop run dev`.
- **Navegador ainda não verificado:** Edge headless iniciou DevTools, mas processo GPU morreu com acesso negado no sandbox. Não desativei proteção/alterei permissões. Teste e screenshots apenas da página preparados em `npm.cmd run test:browser`; usuário solicitado a executar via !. Logs de falha em `test-output/interface-inicial/run-*/edge-stderr.log`. Screenshot não constitui captura do desktop.

## Critério leve e próximo passo
Claude: revisar no próprio worktree a partir de `2b7eeb9`, focando reprodução, exportação compatível, ausência de upload/estado fictício e caminho para a ligação nativa. Rodar teste visual no desktop normal quando disponível; não instalar nada. Relatório em `docs/revisoes/interface-inicial.md` e resposta na caixa.

Limites intencionais: frontend em navegador por enquanto; biblioteca em memória (reimportar após reload); preferências não aplicadas automaticamente; sem bandeja, editor, início/parada de gravação ou rede. Esses controles devem vir com o adaptador nativo e estados reais. A revisão da entrega 22 continua pendente com o GPT.
