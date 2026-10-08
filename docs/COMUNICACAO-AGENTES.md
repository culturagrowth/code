# Comunicação direta entre Claude e GPT

Decisão do usuário em 08/10/2026: os agentes trocam entregas, revisões e pedidos pelos arquivos do projeto.
O usuário não precisa retransmitir essas mensagens entre as sessões.

## Pasta principal e worktrees

A pasta principal permanece **sempre** na branch `claude/sync-gameplay-clip-app-xpgfwx`.
Como a caixa é versionada, trocar essa pasta para uma branch antiga pode remover mensagens e instruções do disco.
Toda implementação e revisão acontece em worktree próprio, criado dentro de `worktrees/`.
O exemplo abaixo mostra a estrutura; o inventário atual vem de `git worktree list`.

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
    ├── gpt-protocolo-2\
    ├── gpt-revisao-<tarefa>\
    └── claude-duoclip-session\
```

Todos os novos worktrees ficam em `duoclip\worktrees\<agente>-<tarefa>`.
As pastas não são repositórios independentes: compartilham branches e histórico, preservando arquivos de trabalho separados.
Use `git worktree move` para reorganizar um worktree. Não mova a pasta principal nem mude de branch na pasta de outra tarefa.
Não faça `git switch`/`checkout` de tarefa na principal. Se ela estiver noutra branch, não conclua que a caixa está vazia;
confira as alterações locais e avise o outro agente antes de organizar o retorno à integração, preservando o trabalho existente.

## Uma caixa física para todas as branches

O caminho canônico é **`C:\Users\bolad\Projetos\duoclip\docs\comunicacao`**.
Mesmo dentro de um worktree, leia e escreva nesse caminho da pasta principal. Uma cópia de `docs/comunicacao` numa branch
é apenas um registro versionado; ela não substitui a caixa atual.
Leia também `AGENTS.md` e `docs/TAREFAS.md` da pasta principal para conhecer as regras e atribuições atuais.
O SPEC e o código da tarefa continuam sendo os da branch correspondente.

Não é necessário push para a outra IA ler mensagens e branches locais no mesmo PC.
Isso não significa que o Git do revisor possa operar no worktree do autor: neste Windows, Claude e Codex usam donos distintos.
Leia os commits pelo repositório principal e crie o worktree de revisão com o usuário do próprio revisor.
Não mude `safe.directory`, permissões, branch ou arquivos do worktree alheio para revisar.

Exemplo de revisão isolada, executado a partir da pasta principal, usando o SHA informado na mensagem:

```powershell
git diff <base>...<sha-entregue>
git worktree add -b gpt/revisao-<tarefa> worktrees/gpt-revisao-<tarefa> <sha-entregue>
```

Os marcadores `<...>` devem ser substituídos antes de executar. O Claude usa `claude/revisao-<tarefa>` e `worktrees/claude-revisao-<tarefa>`.
Essa branch pode receber commits do relatório, sem alterar a branch do implementador.
Em outro computador, o protocolo exige sincronização por Git ou outro mecanismo explicitamente configurado.

## Rotina de cada agente

1. Ao iniciar ou retomar trabalho, consulte sua pasta de mensagens e as respostas aos pedidos que enviou.
2. Entre etapas relevantes e antes de encerrar, consulte novamente a caixa. Não aguarde indefinidamente por outra sessão.
3. Priorize ajustes de suas entregas e revisões atribuídas a você no quadro de tarefas.
4. Antes de começar um pedido, confirme o recebimento em um arquivo novo, destinado ao remetente.
5. Execute a tarefa na branch e no escopo indicados. Publique o resultado e os caminhos das evidências em outro arquivo novo.
6. Atualize o quadro canônico. Quando depender da outra IA, deixe um pedido concreto na caixa em vez de pedir que o usuário o copie.

O quadro e mensagens novas da sua autoria podem ser versionados em commits pequenos de metadados na integração.
Selecione explicitamente esses arquivos; não inclua código, regras, relatórios ou arquivos do outro agente nesse commit.

**Gravar um arquivo não acorda uma sessão parada e não injeta contexto em uma sessão já aberta.**
As instruções orientam agentes ativos a consultar a caixa; não há serviço, monitor de arquivos ou execução automática em segundo plano.
Uma sessão que ainda não leu este protocolo precisa consultá-lo ou ser retomada antes de participar.
Não declare que a outra IA recebeu, revisou ou aprovou algo sem uma resposta assinada por ela.

## Mensagens e respostas

Use um arquivo por mensagem: `AAAA-MM-DD-<remetente>-<sequência>-<assunto>.md`.
Use remetentes `gpt` e `claude` no nome. A sequência começa em `001`, tem pelo menos três dígitos e é **por remetente e por dia**,
considerando as duas pastas da caixa canônica. Reinicie somente ao mudar a data.
Consulte os nomes existentes e escolha o próximo número disponível. Se o nome já existir, escolha outro; nunca sobrescreva para reutilizá-lo.
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
na branch **do revisor** `<revisor>/revisao-<tarefa>`, criada no SHA exato da entrega. A mensagem de resultado inclui branch, commit, caminho e veredito,
para o outro agente ler diretamente com `git show <commit>:docs/revisoes/<tarefa>.md` ou na pasta correspondente.
O autor não edita essa branch ou pasta: corrige na sua branch de implementação e responde aos achados pela caixa.
O revisor confere as correções e atualiza seu relatório. Na integração da tarefa aprovada, inclua o commit do relatório
por cherry-pick de documentação ou procedimento equivalente que preserve autoria e não altere as branches do outro agente.

A memória do projeto contém decisões estáveis e links para o quadro, caixa e relatórios. SHAs atuais, resultados recentes
e estados de recebimento ficam nessas fontes, sem manter cópias voláteis na memória.

Quem implementa não revisa sua própria entrega. Enviar uma mensagem não autoriza gravação de tela ou áudio,
instalação, publicação do Worker ou alteração de tarefas atribuídas ao outro agente.
Nunca coloque chaves, tokens, conteúdo de `.dev.vars` ou URLs assinadas na caixa. Registre resultados sanitizados.

## Evolução desta configuração

Alterações no protocolo e nas regras têm branch própria e revisão cruzada. Publique o pedido pela caixa atual;
as regras canônicas recebem a nova versão com a integração aprovada. Preserve mensagens e respostas novas ao integrar,
sem substituir a caixa por uma cópia antiga da branch.
Arquivos `CLAUDE.local.md` nos worktrees existentes apontam para este protocolo canônico e são ignorados pelo Git.

Fontes: [movimentação de worktrees no Git](https://git-scm.com/docs/git-worktree#_commands),
[instruções locais e importações do Claude Code](https://code.claude.com/docs/en/memory).
