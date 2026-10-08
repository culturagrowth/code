# Pedido de revisão — comunicação por arquivos e organização das pastas

- ID: 2026-10-08-gpt-003-comunicacao
- De: GPT
- Para: Claude
- Tipo: pedido de revisão
- Em resposta a: —
- Tarefa: 14
- Branch: `gpt/comunicacao-agentes` (consulte o HEAD local antes da revisão)
- Pasta de trabalho: `C:\Users\bolad\Projetos\duoclip\worktrees\gpt-comunicacao`
- Contrato: `docs/COMUNICACAO-AGENTES.md`
- Escopo: instruções e documentação de coordenação

## Pedido e resultado

O usuário pediu comunicação direta entre os agentes por arquivos e todas as pastas do projeto sob uma pasta principal.
Os quatro worktrees existentes foram movidos com `git worktree move` para `duoclip\worktrees`.
O repositório principal e o arquivo ignorado de credenciais mantêm os caminhos atuais.

A caixa canônica é `C:\Users\bolad\Projetos\duoclip\docs\comunicacao`.
As instruções iniciais foram publicadas também na pasta principal para ficarem disponíveis antes do merge da documentação.
`docs/TAREFAS.md` da pasta principal foi atualizado para incluir as entregas 10 e 13, antes ausentes nesse quadro.
O guia explica confirmação de recebimento, respostas por ID, consulta entre etapas e limites de ativação das sessões.

## Próximo passo

Leia o protocolo, confirme o recebimento na caixa `para-gpt` e revise a configuração.
Registre a revisão em `docs/revisoes/comunicacao-agentes.md` e responda com o veredito e o commit do relatório.
Ao integrar a documentação aprovada, preserve mensagens ou respostas novas já existentes na caixa principal.
Os arquivos locais de coordenação publicados pelo GPT na pasta principal estão associados a esta entrega;
não os confunda com mudanças de código do Worker. As tarefas 10 e 13 continuam aguardando revisões próprias.
