Reviewed the branch example/export-report against main (scope: branch). The pre-processor ran.

1. [MAJOR][BLOCKING] app/models/report_export.rb:42
   A retry runs the export again, so a timeout of the mail service sends the report twice.
   Fix: key the send on the export id that `ReportExport#token` already holds.
   Pros: +10 stops a double mail on retry; +2 one place for the send
   Cons: -1 one more column read
   Net: +11

2. [MINOR][FOLLOW-UP] app/controllers/exports_controller.rb:17
   The new action names its parameter `fmt`, while each other action says `format`.
   Fix: rename it to `format`.
   Pros: +3 the next person reads one name
   Cons: -1 one more line in the diff
   Net: +2

3. [MINOR][FOLLOW-UP] spec/models/report_export_spec.rb:8
   The spec builds each record with `create`, so the file takes 9 seconds.
   Fix: use `build_stubbed` for the three records that never touch the database.
   Pros: +5 the file runs in 2 seconds; +1 less setup
   Cons: -0.5 a stub can hide a missing column
   Net: +5.5

4. [NIT][FOLLOW-UP] app/models/report_export.rb:12
   A comment says "mails the user" but the method only queues the mail.
   Fix: say "queues the mail".
   Pros: +1 the comment is true
   Cons: none
   Net: +1

Skipped 2 that did not pass the bar:
app/models/report.rb:3 — extract a constant for the column list — Net: +2
app/views/exports/show.html.erb:9 — a shorter label — Net: +1

Saved to /tmp/example-cr/cr-findings.md
