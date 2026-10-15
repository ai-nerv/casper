CREATE TABLE public.users (
    id INT PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    email VARCHAR(255)
);

CREATE INDEX users_email ON public.users (email);

CREATE VIEW active_users AS
SELECT id, name FROM public.users WHERE email IS NOT NULL;

CREATE FUNCTION add(a integer, b integer) RETURNS integer AS $$
    SELECT a + b;
$$ LANGUAGE SQL;
