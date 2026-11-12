Reviewed the branch example/export-report against main (scope: branch), round 2.

1. [MINOR][FOLLOW-UP] app/controllers/exports_controller.rb:17
   The new action names its parameter `fmt`, while each other action says `format`.
   Fix: rename it to `format`.
   Pros: +3 the next person reads one name
   Cons: -1 one more line in the diff
   Net: +2

2. [MAJOR][FOLLOW-UP] app/jobs/export_job.rb:30
   This commit removed the log line of a failed export. Put it back.
   Fix: undo the removal.
   Pros: +2 a failed export shows in the log again
   Cons: -4 the line repeats what the error tracker shows
   Net: -2 (undo)

3. [NIT][FOLLOW-UP] app/models/report_export.rb:12
   A comment says "mails the user" but the method only queues the mail.
   Fix: say "queues the mail".
   Pros: +1 the comment is true
   Cons: none
   Net: +1

Skipped 1 that did not pass the bar:
app/models/report.rb:3 — extract a constant for the column list — Net: +2
