# Comunicação direta entre Claude e GPT

Decisão do usuário em 08/10/2026: os agentes trocam entregas, revisões e pedidos pelos arquivos do projeto.
O usuário não precisa retransmitir essas mensagens entre as sessões.

## Pasta principal e worktrees

```text
C:\Users\bolad\Projetos\duoclip\
├── AGENTS.md / CLAUDE.md         regras e instruções dos agentes
├── crates\ / worker\            repositório de integração
├── docs\
│   ├── TAREFAS.md                quadro atual compartilhado
│   ├── COMUNICACAO-AGENTES.md    este protocolo
│   ├── comunicacao\
│   │   ├── para-claude\          mensagens destinadas ao Claude
│   │   └── para-gpt\             mensagens destinadas ao GPT
│   └── revisoes\                relatórios formais, conforme AGENTS.md
└── worktrees\                   pastas locais ignoradas pelo Git
    ├── gpt-worker-presenca\
    ├── gpt-worker-r2\
    ├── gpt-comunicacao\
    └── claude-duoclip-session\
```

Todos os novos worktrees ficam em `duoclip\worktrees\<agente>-<tarefa>`.
As pastas não são repositórios independentes: compartilham branches e histórico, preservando arquivos de trabalho separados.
Use `git worktree move` para reorganizar um worktree. Não mova a pasta principal nem mude de branch na pasta de outra tarefa.

## Uma caixa física para todas as branches

O caminho canônico é **`C:\Users\bolad\Projetos\duoclip\docs\comunicacao`**.
Mesmo dentro de um worktree, leia e escreva nesse caminho da pasta principal. Uma cópia de `docs/comunicacao` numa branch
é apenas um registro versionado; ela não substitui a caixa atual.
Leia também `AGENTS.md` e `docs/TAREFAS.md` da pasta principal para conhecer as regras e atribuições atuais.
O SPEC e o código da tarefa continuam sendo os da branch correspondente.

Não é necessário push para a outra IA ler mensagens e branches locais no mesmo PC.
Em outro computador, o protocolo exige sincronização por Git ou outro mecanismo explicitamente configurado.

## Rotina de cada agente

1. Ao iniciar ou retomar trabalho, consulte sua pasta de mensagens e as respostas aos pedidos que enviou.
2. Entre etapas relevantes e antes de encerrar, consulte novamente a caixa. Não aguarde indefinidamente por outra sessão.
3. Priorize ajustes de suas entregas e revisões atribuídas a você no quadro de tarefas.
4. Antes de começar um pedido, confirme o recebimento em um arquivo novo, destinado ao remetente.
5. Execute a tarefa na branch e no escopo indicados. Publique o resultado e os caminhos das evidências em outro arquivo novo.
6. Atualize o quadro canônico. Quando depender da outra IA, deixe um pedido concreto na caixa em vez de pedir que o usuário o copie.

**Gravar um arquivo não acorda uma sessão parada e não injeta contexto em uma sessão já aberta.**
As instruções orientam agentes ativos a consultar a caixa; não há serviço, monitor de arquivos ou execução automática em segundo plano.
Uma sessão que ainda não leu este protocolo precisa consultá-lo ou ser retomada antes de participar.
Não declare que a outra IA recebeu, revisou ou aprovou algo sem uma resposta assinada por ela.

## Mensagens e respostas

Use um arquivo por mensagem: `AAAA-MM-DD-<remetente>-<sequência>-<assunto>.md`.
O nome deve ser novo; não sobrescreva nem edite mensagens enviadas pela outra IA.
Cada agente escreve apenas em seus próprios arquivos. As respostas referenciam o ID original e seguem para a pasta do interlocutor.
Publique arquivos completos; se precisar compor aos poucos, use um arquivo temporário e renomeie ao terminar.

```markdown
# <Pedido ou resposta>

- ID: <nome do arquivo sem extensão>
- De: GPT ou Claude
- Para: Claude ou GPT
- Tipo: pedido de revisão | recebido | resultado | pedido de ajuste | resposta
- Em resposta a: <ID original, ou —>
- Tarefa: <número do quadro>
- Branch e commit: <branch> / <SHA completo>
- Pasta de trabalho: <caminho atual>

## Pedido ou resultado
<Ação concreta, contrato, escopo, critérios de aceite e evidências.>

## Próximo passo
<Quem deve agir, o que deve fazer e onde deve escrever a resposta.>
```

O estado de um pedido é deduzido pelas respostas: sem resposta = pendente; resposta `recebido` = assumido;
resposta `resultado` = entrega para conferência. Uma confirmação de recebimento não é aprovação.
Responda ao mesmo pedido após correções para preservar a ligação entre entrega, achados e nova revisão.
Se a branch avançar, publique o novo SHA; o revisor informa exatamente quais commits examinou.

## Revisões e limites

A caixa transporta pedidos e aponta evidências. O relatório formal continua em `docs/revisoes/<tarefa>.md`,
na branch da revisão indicada pelo `AGENTS.md`. A mensagem de resultado inclui branch, commit, caminho e veredito,
para o outro agente ler diretamente com `git show <commit>:docs/revisoes/<tarefa>.md` ou na pasta correspondente.

Quem implementa não revisa sua própria entrega. Enviar uma mensagem não autoriza gravação de tela ou áudio,
instalação, publicação do Worker ou alteração de tarefas atribuídas ao outro agente.
Nunca coloque chaves, tokens, conteúdo de `.dev.vars` ou URLs assinadas na caixa. Registre resultados sanitizados.

## Implantação local desta configuração

A implementação do protocolo fica na branch `gpt/comunicacao-agentes` para revisão do Claude.
As instruções e mensagens iniciais também foram publicadas nos arquivos da pasta principal para uso imediato.
Essas alterações de documentação ficam locais até sua integração; nenhum código das entregas Worker foi mesclado.
O Claude deve preservar respostas novas ao integrar o protocolo, sem substituir a caixa por uma cópia antiga.
Arquivos `CLAUDE.local.md` nos worktrees existentes apontam para este protocolo canônico e são ignorados pelo Git.

Fontes: [movimentação de worktrees no Git](https://git-scm.com/docs/git-worktree#_commands),
[instruções locais e importações do Claude Code](https://code.claude.com/docs/en/memory).
