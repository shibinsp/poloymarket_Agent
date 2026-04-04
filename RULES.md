# Development Rules & Workflow

## Branch Strategy

### Core Rules
1. **NEVER work directly on `main` or `master` branch** — these are protected branches
2. **Every feature/fix gets its own branch** — branched from `main`/`master`
3. **Branch naming convention:**
   - Features: `feature/<short-description>` (e.g., `feature/live-trading-auth`)
   - Bug fixes: `fix/<short-description>` (e.g., `fix/paper-fill-simulator`)
   - Improvements: `improve/<short-description>` (e.g., `improve/parallel-valuation`)
   - Documentation: `docs/<short-description>` (e.g., `docs/api-cost-breakdown`)
   - Refactoring: `refactor/<short-description>` (e.g., `refactor/order-execution-module`)

### Branch Creation
```bash
# Always start from the latest main/master
git checkout main          # or master
git pull origin main       # fetch latest
git checkout -b feature/your-feature-name
```

## Commit Rules

### Commit Message Convention
Follow **Conventional Commits** format:

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

**Types:**
- `feat` — New feature
- `fix` — Bug fix
- `docs` — Documentation only
- `style` — Code style changes (formatting, no logic change)
- `refactor` — Code refactoring (no behavior change)
- `perf` — Performance improvements
- `test` — Adding or updating tests
- `chore` — Build, CI, tooling changes

**Examples:**
```
feat(risk): add correlation risk management module
fix(paper-trading): implement realistic fill simulator
docs(readme): add deployment guide for VPS setup
refactor(valuation): extract prompt builder into separate module
test(kelly): add boundary condition tests for degenerate prices
perf(scanner): parallelize market evaluation with JoinSet
```

### Commit Rules
1. **One logical change per commit** — don't mix unrelated changes
2. **Atomic commits** — each commit should compile and pass tests
3. **Write imperative mood** — "Add feature" not "Added feature" or "Adds feature"
4. **Keep subject line under 72 characters**
5. **Use body for complex changes** — explain WHY, not WHAT (code shows what)

### Commit Workflow
```bash
# Stage specific files
git add src/risk/correlation.rs src/risk/mod.rs

# Review what's staged
git diff --staged

# Commit with conventional message
git commit -m "feat(risk): add correlation risk management module

- Add correlation matrix tracking between markets
- Implement portfolio VaR calculation
- Limit total exposure to correlated buckets (max 15%)"
```

## Pull Request Rules

### PR Creation
1. **Every change go through a PR** — no direct pushes to `main`/`master`
2. **PR title follows conventional commit format**
3. **PR description must include:**
   - What changed and why
   - How to test the changes
   - Any breaking changes
   - Related issues/tickets

### PR Template
```markdown
## Summary
Brief description of what this PR does.

## Changes
- List of key changes

## Testing
- [ ] Unit tests pass (`cargo test`)
- [ ] Clippy passes (`cargo clippy -- -D warnings`)
- [ ] Manual testing steps (if applicable)

## Breaking Changes
- None / List any breaking changes

## Screenshots (if applicable)
- UI changes, dashboard updates, etc.
```

### PR Workflow
```bash
# Push your branch to remote
git push -u origin feature/your-feature-name

# Create PR via GitHub CLI or web interface
gh pr create \
  --base main \
  --head feature/your-feature-name \
  --title "feat(risk): add correlation risk management" \
  --body-file PR_DESCRIPTION.md

# Or create via GitHub web UI
```

### CodeRabbit Review (Mandatory Before Merge)
1. **Install CodeRabbit** — Ensure the [CodeRabbit](https://www.coderabbit.ai/) GitHub App is installed on the repository
2. **Automatic review triggers** — CodeRabbit automatically reviews every PR when it's opened or when new commits are pushed
3. **Review the feedback** — Check the PR comments from `@coderabbitai` for:
   - Bug risks and logic errors
   - Performance issues
   - Security concerns
   - Style and convention violations
   - Suggestions for improvement
4. **Fix all issues** — Address every finding from CodeRabbit before merging:
   - Apply suggested fixes directly or adapt them to your context
   - Commit fixes to the same branch (the PR updates automatically)
   - Re-request review if needed by commenting `@coderabbitai review`
5. **No merge until clean** — Do not merge the PR until CodeRabbit has no outstanding critical issues
6. **Override only with justification** — If you intentionally disagree with a CodeRabbit suggestion, add a comment in the PR explaining why

### PR Review Checklist
Before merging, verify:
- [ ] All CI checks pass (tests, clippy, build)
- [ ] CodeRabbit review complete with no critical issues
- [ ] Code follows project conventions
- [ ] Tests cover new functionality
- [ ] Documentation updated (README, RULES, etc.)
- [ ] No debug code or `println!` statements left in
- [ ] Commit messages follow conventional format

## Merge Rules

### Merging to Main/Master
1. **Squash merge** preferred for clean history (one commit per PR)
2. **Rebase merge** acceptable for feature branches with multiple logical commits
3. **Never force push** to `main`/`master`
4. **Delete feature branch** after merge

### Post-Merge
```bash
# After PR is merged
git checkout main
git pull origin main
git branch -d feature/your-feature-name      # delete local
git push origin --delete feature/your-feature-name  # delete remote
```

## Code Quality Rules

### Rust-Specific
1. **Always use `rust_decimal::Decimal` for money** — never `f64`
2. **Run `cargo clippy -- -D warnings`** before committing
3. **Run `cargo test`** — all tests must pass
4. **Run `cargo fmt`** — consistent formatting
5. **No `unwrap()` in production code** — use `?` operator or proper error handling
6. **Use `tracing` for logging** — not `println!`
7. **Async functions must be `#[instrument]`** for tracing

### Testing
1. **Every new feature needs tests** — minimum 80% coverage target
2. **Unit tests in same file** as the code (`#[cfg(test)]` module)
3. **Integration tests in `tests/` directory**
4. **Test edge cases** — empty inputs, boundaries, error conditions
5. **Tests must be deterministic** — no flaky tests

### Error Handling
1. **Use `thiserror` for library errors**
2. **Use `anyhow` for application errors**
3. **Never swallow errors** — log or propagate
4. **Error messages should be actionable** — tell the user what went wrong and how to fix

## Feature Development Workflow

### Step-by-Step for Each Feature
```bash
# 1. Create feature branch
git checkout main && git pull origin main
git checkout -b feature/feature-name

# 2. Implement incrementally with commits
#    - Write tests first (TDD preferred)
#    - Implement minimum viable code
#    - Refactor and improve
git add <files>
git commit -m "feat(scope): implement feature part 1"

# 3. Run quality checks
cargo fmt
cargo clippy -- -D warnings
cargo test

# 4. Push and create PR
git push -u origin feature/feature-name
gh pr create --base main --title "feat(scope): feature description"

# 5. After review and merge, clean up
git checkout main && git pull origin main
git branch -d feature/feature-name
```

## File Organization Rules

### Module Structure
```
src/
├── main.rs              # Entry point only
├── lib.rs               # Module declarations only
├── config.rs            # Configuration loading
└── <domain>/
    ├── mod.rs           # Module exports
    ├── models.rs        # Domain types
    ├── service.rs       # Business logic
    └── <feature>.rs     # Feature-specific implementation
```

### File Rules
1. **Keep files under 500 lines** — split if larger
2. **One responsibility per file** — clear, single purpose
3. **Public API in `mod.rs`** — re-export from submodules
4. **Tests at bottom of file** — in `#[cfg(test)]` module
5. **Documentation comments** — `///` for public API, `//` for internal notes

## Configuration Rules

1. **Tunable parameters in `config/default.toml`** — not hardcoded
2. **Secrets in environment variables only** — never in config files
3. **`.env` in `.gitignore`** — use `.env.example` for template
4. **Document all config options** — in README and config file comments

## Database Rules

1. **Migrations are append-only** — never modify existing migration files
2. **New migration for every schema change** — even small ones
3. **Use `rust_decimal` for all monetary columns** — store as TEXT
4. **Index foreign keys and query columns** — performance matters
5. **Test migrations** — verify up and down work correctly

## Deployment Rules

1. **Paper trade minimum 48-72 hours** before going live
2. **Backtest with 500+ trades** before paper trading
3. **Monitor health endpoint** — set up alerts for downtime
4. **Log structured JSON** — enable log aggregation
5. **Track API costs daily** — stay within budget

## Emergency Procedures

### If Agent Goes Wrong
```bash
# 1. Stop the agent immediately
sudo systemctl stop polymarket-agent

# 2. Check current state
curl http://localhost:8080/api/health
curl http://localhost:8080/api/metrics

# 3. Review recent trades
curl http://localhost:8080/api/trades | jq '.[-10:]'

# 4. Check logs
sudo journalctl -u polymarket-agent -n 100 --no-pager

# 5. If live trading, manually check Polymarket positions
#    and cancel any open orders via web interface
```

### Rollback Procedure
```bash
# Revert to previous version
git checkout <last-good-commit>
cargo build --release
sudo systemctl restart polymarket-agent
```

## CI/CD Rules (When Implemented)

1. **All PRs must pass CI** — no merging on red CI
2. **Required checks:**
   - `cargo test` — all tests pass
   - `cargo clippy -- -D warnings` — no lint warnings
   - `cargo build` — compiles successfully
   - `cargo fmt --check` — formatting correct
3. **Auto-deploy on merge to main** — only after all checks pass

## Review Guidelines

### For Reviewers
- [ ] Code is correct and complete
- [ ] Tests cover the changes
- [ ] Error handling is appropriate
- [ ] No performance regressions
- [ ] Follows project conventions
- [ ] Clear, readable code
- [ ] Documentation updated

### For Authors
- Respond to all review comments
- Push fixes as new commits (don't force push during review)
- Request re-review after addressing comments
- Be respectful and constructive

## Quick Reference

### Common Commands
```bash
# Start new feature
git checkout main && git pull origin main
git checkout -b feature/name

# Quality check before commit
cargo fmt && cargo clippy -- -D warnings && cargo test

# Commit changes
git add <files> && git commit -m "type(scope): description"

# Push and create PR
git push -u origin feature/name
gh pr create --base main --title "type(scope): description"

# After merge
git checkout main && git pull origin main
git branch -d feature/name
```

### Type/Scope Cheat Sheet
| Type | When to Use |
|------|-------------|
| `feat` | New user-facing functionality |
| `fix` | Bug fixes |
| `docs` | Documentation only |
| `style` | Formatting, no code change |
| `refactor` | Code restructuring, same behavior |
| `perf` | Performance improvements |
| `test` | Test additions or fixes |
| `chore` | Build, CI, tooling |

| Scope | Module |
|-------|--------|
| `agent` | Agent lifecycle, self-funding |
| `scanner` | Market discovery |
| `valuation` | Claude API, fair value, edge |
| `risk` | Kelly, portfolio, limits, exit |
| `execution` | Orders, fills, resolution, wallet |
| `monitoring` | Metrics, alerts, dashboard |
| `backtest` | Backtesting engine |
| `data` | External data sources |
| `db` | Database operations |
| `config` | Configuration loading |
