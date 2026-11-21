-- Sign: a request is Completed only with its sealed PDF and that file's SHA-256. Sealing sets all three in one
-- statement (seal.rs `complete`); no other write, by the application or by hand, may complete a request without them.
--
-- `not valid`: the rows already there are not checked (a workspace may hold a Completed request an older release left
-- without its seal; checking would fail the upgrade and hold writes while it ran), every new and changed row is. Upkeep
-- seals those old requests (seal.rs) and then validates the constraint once none is left (upkeep.rs `check_seals`).
alter table e_sign_request add constraint sign_completed_is_sealed
  check (status is distinct from 'Completed' or (sealed_document is not null and sealed_sha256 is not null)) not valid;
