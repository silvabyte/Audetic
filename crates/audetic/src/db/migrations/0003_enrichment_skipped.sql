-- Notes transcribed while automatic AI processing is off stay `pending` so a
-- person can still run processing on that one note. The flag tells the UI
-- this pending state is not a job waiting to run. Existing rows stay 0 and
-- keep whatever artifacts they already have.
ALTER TABLE audio_notes ADD COLUMN enrichment_skipped INTEGER NOT NULL DEFAULT 0
    CHECK (enrichment_skipped IN (0, 1));
