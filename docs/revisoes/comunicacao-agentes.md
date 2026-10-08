# Revisão — tarefa nº 14: comunicação direta por arquivos e organização dos worktrees

- Branch: `gpt/comunicacao-agentes` · commits revisados: `a643c03..25e0e3682d596df97c53f659890c2ff3c47f5ca1` (`a4c3ed5`, `25e0e36`)
- Implementado por: GPT · Revisado por: Claude · Data: 2026-10-08
- Checagens: só documentação e `.gitignore`; não há código a compilar. Conferi que as cópias locais da pasta principal
  (`docs/COMUNICACAO-AGENTES.md`, `docs/comunicacao/**`, `AGENTS.md`, `CLAUDE.md`, `.gitignore`, `docs/MEMORIA-DO-PROJETO.md`,
  `docs/PROMPT-DELEGAR-TAREFA.md`) são **idênticas** à branch. A única diferença é o `docs/TAREFAS.md` local, com as linhas 10, 13 e 14,
  como o pedido descreve. `git worktree list` mostra os 4 worktrees em `worktrees/` e os vínculos funcionam a partir do repositório principal.
- Não verificado: se a sessão do GPT (usuário `CodexSandboxOffline`) consegue ler os arquivos que o Claude cria (usuário `bolad`) em `para-gpt`.

## Achados

### 1. [importante] Os arquivos da caixa somem do disco se alguém trocar de branch na pasta principal
- Onde: `docs/COMUNICACAO-AGENTES.md`, seções "Pasta principal e worktrees" e "Uma caixa física para todas as branches"
- Problema: depois do merge, `docs/comunicacao/**` passa a ser **rastreado** na branch de integração. Se a pasta principal fizer
  `git switch` para uma branch que não tem esses arquivos (qualquer branch de tarefa criada antes do merge), o Git os remove do disco.
  As mensagens novas, ainda não commitadas, continuam lá, mas as já commitadas somem. A "caixa canônica" fica num estado que depende da
  branch em que a pasta principal está.
- Cenário que falha: com o protocolo integrado, `git switch claude/fase-b1` na pasta principal (eu mesmo fiz isso hoje, antes do protocolo,
  para criar essa branch) → `docs/comunicacao/para-claude/2026-10-08-gpt-00*.md` desaparecem → um agente que consulta a caixa nesse momento
  conclui que não há pedidos.
- Sugestão: regra explícita no protocolo e no `AGENTS.md`: **a pasta principal fica sempre na branch de integração**; todo trabalho de
  tarefa (inclusive criar branch) acontece em `worktrees/`. Se possível, `git switch` na pasta principal só para voltar à integração.

### 2. [importante] Os worktrees de um agente ficam inacessíveis ao Git do outro (dono diferente no Windows)
- Onde: `docs/COMUNICACAO-AGENTES.md`, "Não é necessário push para a outra IA ler mensagens e branches locais no mesmo PC"
- Problema: os worktrees `worktrees/gpt-*` pertencem ao usuário `Nico/CodexSandboxOffline`, e o Git do Claude roda como `NICO/bolad`.
  Qualquer `git` nessas pastas falha com `detected dubious ownership`. A frase acima é verdadeira para as **branches** (lidas pelo
  repositório principal), mas não para as **pastas** dos worktrees, e o pedido de revisão aponta "Pasta de trabalho: worktrees/gpt-…".
- Cenário que falha: `git -C worktrees/gpt-worker-presenca rev-parse HEAD` → `fatal: detected dubious ownership`.
- Sugestão: documentar que o revisor **lê as branches pelo repositório principal** (`git log/diff/show <branch>`) e roda as checagens
  num **worktree próprio** criado no commit revisado (`git worktree add -b <revisor>/revisao-<tarefa> worktrees/<revisor>-revisao-<tarefa> <sha>`),
  sem tocar na pasta do outro. Não sugerir `safe.directory` sem o usuário decidir (é configuração global do Git).

### 3. [menor] Onde fica o relatório de revisão: "na branch revisada" conflita com as regras de pasta
- Onde: `AGENTS.md`, item 5, e `docs/revisoes/README.md` ("criado pelo revisor na branch revisada")
- Problema: commitar na branch do outro agente move a branch que está aberta no worktree dele (o worktree dele passa a ver arquivos
  "sumindo" e o `HEAD` andar sozinho), e o protocolo pede para não mexer na pasta de outra tarefa.
- Sugestão: o relatório vai numa branch do revisor, `<revisor>/revisao-<tarefa>`, criada a partir do commit revisado e contendo só o
  arquivo de revisão. É o que fiz nesta revisão (`claude/revisao-comunicacao`, `claude/revisao-worker-presenca`, `claude/revisao-worker-r2`).
  O dono lê com `git show <branch>:docs/revisoes/<tarefa>.md`, e o arquivo entra na integração junto com o merge da tarefa.

### 4. [menor] A memória do projeto passa a guardar estado volátil
- Onde: `docs/MEMORIA-DO-PROJETO.md`, novo bloco em "Em andamento / pendente" (SHAs, contagens de testes, "aguardam respostas")
- Problema: isso duplica o `docs/TAREFAS.md` e a caixa, e fica velho a cada commit (os SHAs já mudaram uma vez hoje).
- Sugestão: manter na memória só as decisões (comunicação por arquivos, pasta única de worktrees) e apontar para o quadro e a caixa para o estado.

### 5. [menor] Numeração das mensagens
- Onde: `docs/COMUNICACAO-AGENTES.md`, "Use um arquivo por mensagem: `AAAA-MM-DD-<remetente>-<sequência>-<assunto>.md`"
- Problema: não diz se a sequência é por remetente e por dia. Como o remetente está no nome, não há colisão entre agentes, mas convém fixar.
- Sugestão: "sequência de 3 dígitos por remetente, reiniciando a cada dia". É o que usei (`2026-10-08-claude-001…`).

### 6. [menor] Conflito previsível no `.gitignore`
- Onde: `.gitignore`
- Problema: a branch `claude/fase-b1` também acrescenta uma linha no fim (`/.claude/`). O merge das duas dá um conflito trivial.
- Sugestão: nada a fazer na branch; resolvo no merge mantendo as duas regras.

## O que está bom
- A caixa com um arquivo por mensagem, sem edição de mensagens alheias e com respostas por ID, é simples e auditável.
- O texto é honesto sobre o limite: gravar um arquivo não acorda outra sessão, e recebimento só vale com resposta.
- Regras de segurança corretas: sem segredos na caixa, mensagem não autoriza gravação, instalação ou deploy.
- `CLAUDE.local.md` e `worktrees/` ignorados pelo Git.

## Veredito
**Aprovado com ressalvas** — o protocolo pode ser integrado já; os achados 1 a 3 devem virar texto no protocolo e no `AGENTS.md`
num commit de acompanhamento do GPT (sugestão: tarefa 14, segunda rodada), porque 1 e 2 já causaram problemas reais hoje.
