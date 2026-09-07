# Model evaluation

Comparing reasoning models on **your** denials, not on benchmarks. What matters
for this application is not general capability: it is whether the model returns
parseable JSON, whether its category and its recommended action agree, and
whether it gets patient responsibility right — that last one being the
difference between billing a patient and writing off money you could collect.

## Running it

One model at a time, because the 16 GB card holds only one.

```bash
./scripts/eval_model.sh run                      # with model A loaded
llm-model 38                                     # on the llama.cpp host
./scripts/eval_model.sh run --denial-ids $(./scripts/eval_model.sh ids eval/results/A.json)
python3 scripts/eval_model.py compare eval/results/A.json eval/results/B.json
```

Passing `--denial-ids` from the first run is what makes the second comparable;
without it a later run may pick up a different set of denials and the
comparison is meaningless.

**It writes nothing to the database.** It reads denials, asks the rag-engine to
build the same prompt the application would, and calls llama.cpp directly
rather than going through `/analyses/generate` — so evaluating a model does not
litter the database with analyses nobody asked for.

## Results, 2026-09-06, 8 denials

|                        | Qwen3.6-35B-A3B | Qwen3.8-27B |
|------------------------|-----------------|-------------|
| valid JSON             | 100%            | 100%        |
| action in enum         | 100%            | 100%        |
| category/action agree  | 100%            | 100%        |
| PR not written off     | 100% (1/1)      | 100% (1/1)  |
| appeal flag consistent | 100%            | 100%        |
| has steps              | 100%            | 100%        |
| median seconds         | 18.8            | 21.4        |
| median output tokens   | 397             | 528         |
| errors / truncated     | 0 / 0           | 0 / 0       |

Both were correct on every structural check, so **this sample does not separate
them**. Eight denials from one payer is far too small to conclude anything about
quality; it does establish that the 27B is a safe drop-in — no truncation at
16k context, no JSON failures, no enum violations.

They disagreed on exactly one denial:

    PCN10005  CARC 197 (precertification absent)
      Qwen3.6 -> clinical_documentation
      Qwen3.8 -> appeal

Neither is wrong. A missing prior authorisation can be resolved by supplying a
retro-authorisation, or by contesting the denial — which is right depends on the
payer's retro-auth policy, exactly the sort of thing that should be in the
knowledge base rather than inferred.

The 27B is slower (~14% on the median, and its slowest case was 45s against
26s) and more verbose. That is expected: it is a **dense** 27B against a MoE
with ~3B active parameters, so it does roughly an order of magnitude more
compute per token.
