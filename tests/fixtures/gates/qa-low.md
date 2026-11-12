# QA Report: Example Shop

| Field | Value |
|-------|-------|
| **Date** | 2026-11-16 |
| **URL** | http://localhost:3000 |
| **Branch** | example/export-report |
| **Commit** | 9f3e21c (2026-11-16) |
| **PR** | — |
| **Tier** | Standard |
| **Scope** | The export of a report |
| **Pages visited** | 6 |
| **Screenshots** | 9 |
| **Framework** | Rails |

## Health Score: 94/100

| Category | Score |
|----------|-------|
| Console | 100 |
| Links | 100 |
| Visual | 94 |
| Functional | 100 |
| UX | 97 |
| Performance | 100 |
| Accessibility | 100 |

## Top 3 Things to Fix

1. **ISSUE-001: The export button has a typo** — it says "Exprot".
2. **ISSUE-002: The hover color of the export button differs** — it uses the old gray.

## Summary

| Severity | Count |
|----------|-------|
| Critical | 0 |
| High | 0 |
| Medium | 0 |
| Low | 2 |
| **Total** | **2** |

## Issues

### ISSUE-001: The export button has a typo

| Field | Value |
|-------|-------|
| **Severity** | low |
| **Category** | content |
| **URL** | http://localhost:3000/reports/7 |

**Description:** The button says "Exprot". Expected "Export".

**Repro Steps:**

1. Navigate to http://localhost:3000/reports/7
2. **Observe:** the label of the button.

---

### ISSUE-002: The hover color of the export button differs

| Field | Value |
|-------|-------|
| **Severity** | low |
| **Category** | visual |
| **URL** | http://localhost:3000/reports/7 |

**Description:** On hover the button turns the old gray, while each other button turns blue.

---

## Fixes Applied (if applicable)

| Issue | Fix Status | Commit | Files Changed |
|-------|-----------|--------|---------------|
| ISSUE-001 | deferred | — | — |

### Before/After Evidence

#### ISSUE-001: The export button has a typo
**Before:** ![Before](screenshots/issue-001-result.jpg)

## Ship Readiness

| Metric | Value |
|--------|-------|
| Health score | 94 → 94 (0) |
| Issues found | 2 |
| Fixes applied | 0 |
| Deferred | 1 |

**PR Summary:** "QA found 2 issues, fixed 0, health score 94 → 94."
