# Pesquisa bruta e relatórios dos agentes

Saídas completas, em JSON, dos workflows de agentes rodados na sessão de 07–08/10/2026. Estão guardadas aqui para a migração não
perder nada. Cada afirmação traz fontes (URLs e linhas de código) e um nível de confiança:
- `primary`: fonte oficial ou código;
- `secondary`: terceiros;
- `inferred`: dedução do agente.

| Arquivo | O que tem |
|---|---|
| `2026-10-07-rodada2-captura-relogio-bucket-posroll.json` | Rodada 2: métodos de captura de OBS (código-fonte), Medal e outros; relógio global (W32Time, NTP.br, Cloudflare NTS, estimador); bucket (R2, S3, GCS, Supabase, B2, Wasabi); pós-roll. Inclui a verificação adversarial e a crítica de lacunas. |
| `2026-10-08-rodada3-captura-sem-borda-e-hook.json` | Rodada 3 (parcial; o restante caiu no limite de sessão): por que o Medal não tem borda no Windows 10, a borda do WGC no Win10, a viabilidade do hook e a tabela de anti-cheat por jogo |
| `2026-10-08-fase-a-relatorios-dos-agentes.json` | Fase A: relatório de implementação e revisão adversarial de proto, crypto, worker, clock e buffer |
| `2026-10-08-fase-b1-relatorios-dos-agentes.json` | Fase B1: só o gamesdb terminou. Mux, áudio, encode e a revisão do gamesdb caíram no limite de sessão. |

Os scripts que geraram esses resultados estão em [`tools/agent-workflows/`](../../tools/agent-workflows/).
