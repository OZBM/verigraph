# R-09 · Token efficiency and model competence

**Question.** Is VG-Sym cheaper in tokens than Python or Rust, and how well do models write it?
**Why it is research.** The source spec's 70–85% saving was never measured; models have no pretraining on VG-Sym.
**Fallback that ships.** Current syntax and the in-context synthesis skill.  **Time-box:** 4 weeks.  **Starts after:** P5.

## Proposed solution

Measure before claiming. Write each benchmark task in VG-Sym, Python and Rust; count tokens under several tokenizers; and score verified success. Two published results frame the bets: an AI-oriented grammar for Python (SimPy) cut tokens by 13.5% for CodeLlama and 10.4% for GPT-4 with no loss of accuracy, a far cry from the source spec's 70–85%; and on the vericoding benchmark, off-the-shelf models produced verified code for 82% of Dafny specs, 44% of Verus and 27% of Lean, so a language whose proofs an SMT solver discharges automatically is the easiest target for models, which is VeriGraph's bet. Then tune the surface syntax for tokens multiplied by repair rounds, and grow a verified-only training corpus from OmniLift output for a small fine-tuned model.

## Go if

The measured ratio is published whatever it is; the syntax variant with the lowest tokens × rounds is chosen; the fine-tuned 7B model reaches 70% of a frontier hosted model's verified success.

## Starting points

- [AI Coders Are Among Us: Rethinking Programming Language Grammar Towards Efficient Code Generation](https://arxiv.org/abs/2404.16333) (Sun et al., ISSTA 2024): SimPy
- [A benchmark for vericoding: formally verified program synthesis](https://arxiv.org/abs/2509.22908) (Bursuc et al., 2025); [VERINA](https://arxiv.org/abs/2505.23135), [CLEVER](https://arxiv.org/abs/2505.13938), [DafnyBench](https://arxiv.org/abs/2406.08467)
- Code-LLM tokenization studies; low-resource programming language adaptation
- Synthetic data for code models; LoRA fine-tuning

## Prompt

```text
Run research track R-09 (token efficiency and model competence on VG-Sym) following docs/research/PROTOCOL.md.
Question: is VG-Sym cheaper in tokens than Python or Rust, and how well do models write it?
Build behind feature r-09: (1) 50 tasks with reference solutions in VG-Sym, Python and Rust; (2) token counts under at least four tokenizers (OpenAI o200k, Llama, Qwen, Gemma); (3) surface-syntax variants (keyword versus symbol ops, label length); (4) a verified-only synthetic corpus from OmniLift output and mutations; (5) a LoRA fine-tune of one open 7B code model.
Measure: tokens per task by language and tokenizer, verified pass@1 and pass@5, repair rounds, tokens times rounds per variant, fine-tuned versus in-context performance.
Read first: code-LLM tokenization studies, low-resource programming language adaptation, synthetic data for code models.
Go if: publish the measured ratio whatever it is, pick the syntax variant with the lowest tokens times rounds, and the fine-tuned 7B reaches 70% of a frontier hosted model's verified success. Otherwise keep the current syntax and the in-context skill.
```
