# Workflows de agentes (Claude Code)

Scripts da ferramenta **Workflow** do Claude Code usados para pesquisar e implementar o DuoClip.
Eles servem para refazer ou continuar o trabalho em outro ambiente. Os prompts têm as regras de cada etapa.

| Script | O que faz | Estado |
|---|---|---|
| `duoclip-research-round2.js` | Pesquisa: captura (OBS/Medal/outros), relógio global, bucket e pós-roll, com verificação adversarial | ✅ Concluído |
| `duoclip-borderless-win10-capture.js` | Pesquisa: captura sem borda no Win10, Desktop Duplication × DWM shared surface × hook, anti-cheat por jogo | ⚠️ Parcial: as partes "border" e "hook" terminaram; "dda", "dwm", as verificações e a crítica caíram no limite de sessão |
| `duoclip-phase-a-implement.js` | Implementa e revisa proto+crypto e worker (sonnet), clock e buffer (opus) | ✅ Concluído |
| `duoclip-phase-b1-implement.js` | Implementa e revisa gamesdb (sonnet), mux, áudio e encode (opus) | ⚠️ Só a implementação do gamesdb terminou. Mux e áudio ficaram com código parcial (compila e os testes passam), e o encode não começou. |

Como usar, dentro do Claude Code com a ferramenta Workflow:
- passe o script (ou `scriptPath`);
- nas pesquisas, passe `args: {"scratch": "<pasta temporária>"}`;
- os caminhos dos scripts assumem o repositório em `/home/user/code`; ajuste a constante `ROOT` se for diferente.

Divisão de modelos que o usuário pediu: **sonnet** para código simples e **opus** para as partes difíceis, sempre com revisão adversarial.
