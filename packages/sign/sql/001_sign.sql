-- Sign: the audit trail is append-only. Rows are never changed, by the application or by hand;
-- removing one breaks the SHA-256 chain that Verify Integrity recomputes.

create or replace trigger sign_event_append_only before update on e_sign_event
  for each row execute function ferrum_forbid_change();
create or replace trigger sign_event_no_truncate before truncate on e_sign_event
  for each statement execute function ferrum_forbid_change();
