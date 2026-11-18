# The Dashboard, in a real browser

Taken by hand for the ticket "The Dashboard" at commit `7cca890`, with Firefox 159 (headless, driven over Marionette) against the built `vm` beside a Postgres 18 and a small OIDC provider on the same machine. The people, Applications and Resources are made up for the walk.

| File | What it shows |
|---|---|
| `1-list.png` | The list of a person signed in through the identity provider: what they contribute to, then what is shared with them, each Resource marked with what they hold and whom to ask. The first Application has a 63-character name and a description of one long address, to show that the page holds to its window. |
| `2-application-shared.png` | One Application's page just after its Contributor shared it by the form, by email address. |
| `3-refused.png` | The same form with an address nobody here has: the refusal, in words for a person. |
| `4-list-of-the-person-shared-with.png` | The Dashboard of the person it was shared with, at their next request. They open it from there without the identity provider being asked again. |
| `5-application-platform-administrator.png` | The page as a Platform administrator who cannot open the Application reads it. |
| `6-list-dark.png` | The list with the system's dark scheme. |
| `7-list-narrow.png` | The list in a window of 500 CSS pixels (the narrowest this headless Firefox gives). |
