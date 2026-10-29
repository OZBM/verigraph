# Research protocol

Every research track follows these steps. The track's own file gives the question, the proposed solution, the success criteria and the prompt.

1. Work on a `research/r-xx` branch behind an `r-xx` cargo feature (or a separate `research/r-xx/` folder for Python); main's gates must stay green.
2. Survey the listed starting points plus at least five newer sources, with links. Prefer primary sources: papers, official repositories, documentation.
3. Write hypotheses and success metrics in the report before writing code.
4. Prototype the proposed solution. If the survey turns up a better one, build that instead and say why.
5. Run experiments with fixed seeds and logged configurations; raw results go in `research/r-xx/results/`, never only in prose.
6. Write `docs/research/R-xx-report.md`:
   - Question and hypotheses
   - Survey (links, one line each on what matters)
   - What was built, and how to run it
   - Results: tables with numbers, including failures
   - Decision: go or no-go against the track's criteria
   - Integration plan (on go), or what would change the answer (on no-go)
7. On go, open an integration change that passes every gate. On no-go, the fallback stays. When the time-box ends, report what is known, even if incomplete.

Rules from AGENTS.md still apply: never fake numbers, never weaken a check, ask the owner before any paid service.
