-- What part A found the design needs, applied over design.sql as a1_vm.
--
-- 1. The capture runs as a1_vm and to_jsonb calls a cast to json of a
--    user-defined type if one exists (a1_mig can make one for an enum it
--    owns), so the capture refuses to convert while such a cast exists
--    whose function is not C or internal (only a superuser or a trusted
--    extension's script makes those).
-- 2. What to_jsonb prints depends on the session's settings, which the
--    session (Application SQL, a migration) controls: pin them for the
--    capture.
CREATE OR REPLACE FUNCTION vm.capture_guard() RETURNS void
LANGUAGE plpgsql VOLATILE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
BEGIN
    IF EXISTS (SELECT FROM pg_catalog.pg_cast c
                 JOIN pg_catalog.pg_proc p ON p.oid = c.castfunc
                 JOIN pg_catalog.pg_language l ON l.oid = p.prolang
                WHERE c.casttarget IN ('json'::pg_catalog.regtype, 'jsonb'::pg_catalog.regtype)
                  AND c.castsource >= 16384 AND l.lanname NOT IN ('c', 'internal')) THEN
        RAISE EXCEPTION USING ERRCODE = 'VM003',
            MESSAGE = 'a cast to json of a type of the Application exists: the capture refuses to run it';
    END IF;
END
$f$;
REVOKE ALL ON FUNCTION vm.capture_guard() FROM PUBLIC;
ALTER FUNCTION vm.capture() SET TimeZone = 'UTC';
ALTER FUNCTION vm.capture() SET extra_float_digits = 1;
ALTER FUNCTION vm.capture() SET IntervalStyle = 'postgres';
ALTER FUNCTION vm.capture() SET bytea_output = 'hex';
ALTER FUNCTION vm.capture() SET DateStyle = 'ISO, YMD';
