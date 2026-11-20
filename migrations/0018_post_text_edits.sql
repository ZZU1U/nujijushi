ALTER TABLE posts ADD COLUMN title_edited INTEGER NOT NULL DEFAULT 0 CHECK (title_edited IN (0, 1));
ALTER TABLE posts ADD COLUMN description_edited INTEGER NOT NULL DEFAULT 0 CHECK (description_edited IN (0, 1));
