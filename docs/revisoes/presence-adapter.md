# Revisão cruzada — adaptador de presença (tarefa 17)

Revisor: GPT, 08/10/2026. Pedido: `2026-10-08-claude-021-pedido-revisao-presence-adapter`.
SHA examinado: `0e6e22f0f7aaab6717b1f8b08e81c5a2dfb50a9a`, branch `claude/presence-adapter`; base `c1ccfb6`.
Branch própria: `gpt/revisao-presence-adapter`, em `worktrees/gpt-revisao-presence-adapter`.
Nenhuma implementação alterada pelo GPT.

**Veredito: aprovado.** Não identifiquei achado bloqueante nos fluxos normais examinados. Revisão proporcional à decisão 19 do usuário: funcionamento do grupo e isolamento primeiro.

## Pontos conferidos

- Corpo/resposta comparados com `worker/src/validate.ts`, `worker/src/routes.ts` e `worker/SPEC.md`: campos obrigatórios, nulls, UUIDs, inteiros seguros, retratos completos e ordenação de execução/seq são compatíveis no fluxo normal. Worker timestamps não viram relógio local. A comparação foi de código/contrato; a simulação Rust usa um Worker falso, não o serviço publicado.
- Abrir/fechar o jogo: anúncio de mudança, heartbeat de 30 s quando ativo e anúncio ocioso seguido de silêncio. Falhas de rede e 409 respeitam espera; não há segundo pedido simultâneo nem laço de tentativas.
- Reinício: `online_since_ms` cresce com o estado persistido; testes de reinício com estado salvo passam sem 409. O app ainda deverá salvar esse estado ao iniciar. Sem ele e com relógio atrasado, o comportamento documentado é aguardar a expiração da presença anterior.
- Saída de um amigo: `apply_snapshot` esquece ausentes e não renova frescor por par repetido; TTL local continua funcionando quando não chegam respostas. Os cenários de queda/retorno e reinício rápido do jogo passaram.
- Isolamento: `apply_snapshot` continua encaminhando somente membros aceitos pelo SessionManager. Candidatos, `clip_targets` e `check_request` conservam as regras da sessão e do grupo. Simulações com dois grupos, troca de grupo e nove dispositivos/até oito lugares passaram, incluindo os cenários de agreement alimentados por retratos Worker.

## Decisões de integração aceitas

Tempo da sessão baseado no identificador da execução mais tempo local decorrido, intervalo limitado por TTL/3 e graça de troca mínima de 65 s estão documentados no SPEC e cobertos pelos cenários normais. A graça evita encerrar a sessão durante um reinício breve do jogo; implica esperar esse período antes de trocar automaticamente para outro grupo quando os amigos saem. Aceito esse comportamento nesta entrega.

Na integração do app: chamar `tick`/`poll` regularmente, usar os wrappers de tempo do PresenceClient, salvar `persisted_state`, carregar membership e reportar conclusão/falha de cada request. HTTP e assinatura ainda pertencem à camada de transporte futura; este crate entrega a lógica, sem fazer rede sozinho. A aprovação não significa que gravação e troca de clipes entre PCs já estejam prontas.

## Validação independente

- Fmt e clippy `--workspace --all-targets -- -D warnings`: passaram.
- `cargo test --workspace`: **567 passaram, 0 falharam, 24 ignorados**.
- Check `--workspace --all-targets --target x86_64-pc-windows-gnu`: passou.
- Simulações de abrir/fechar jogo, reiniciar, desconectar/retornar, dois grupos e limite de lugares executadas na suíte, sem captura ou serviço externo.

Logs locais ignorados: `test-output/review-presence/{fmt,clippy,tests,gnu}.log`. Não repeti testes de hardware das bases já aprovadas. Nenhuma instalação, gravação ou alteração do Worker remoto.

Claude pode integrar o SHA examinado e este relatório preservando autoria. Nenhuma correção solicitada nesta rodada.
