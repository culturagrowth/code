# CLAUDE.md — DuoClip

As regras do projeto são compartilhadas com outras IAs e ficam no `AGENTS.md` (importado abaixo). **Leia-o inteiro.**

@AGENTS.md

@docs/COMUNICACAO-AGENTES.md

## Específico do Claude Code

- Na coluna "Dono" de `docs/TAREFAS.md`, o seu nome é **Claude**, e suas branches são `claude/<tarefa>`.
- Consulte as mensagens em `C:\Users\bolad\Projetos\duoclip\docs\comunicacao\para-claude` antes de começar, entre etapas e antes de encerrar.
  Publique confirmações, revisões e pedidos ao GPT em `...\para-gpt`, seguindo o protocolo compartilhado.
  Ao entregar uma tarefa, publique o pedido de revisão diretamente nessa caixa; o usuário não precisa retransmitir a entrega.
- Ao usar subagentes ou a ferramenta Workflow para **implementar**: os modelos mais rápidos (Sonnet/Haiku) ficam com o código simples e
  bem especificado, os mais avançados (Opus) com as partes difíceis.
- **Revisão é do GPT, não de subagentes do Claude** (decisão do usuário em 08/10/2026, ver `AGENTS.md`, item 5). Ao terminar uma tarefa,
  deixe-a na branch `claude/<tarefa>` com status `em revisão` e publique o pedido de revisão na caixa compartilhada.
  Quando o GPT entregar uma tarefa, você é o revisor: escreva `docs/revisoes/<tarefa>.md` e não corrija o código dele.
- Os scripts de workflow ficam em `tools/agent-workflows/` (só funcionam no Claude Code).
- Quando o usuário delegar uma tarefa ao GPT, publique o pedido na caixa compartilhada e registre a tarefa em `docs/TAREFAS.md`
  com dono **GPT** e revisor **Claude**.
