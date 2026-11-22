# Smart View — vue intelligente du code pour une forge git

Mis à jour le 2026-11-22

## L'idée

La Smart View analyse le code d'un repo et le découpe en blocs : build Docker, partie Web, BDD, API, CI, etc. Chaque bloc affiche ses dépendances et les personnes du projet qui le connaissent bien, pour savoir à qui demander des infos.

Elle répond à la vraie question qu'on se pose en arrivant sur un repo : « c'est quoi les morceaux, et à qui je demande ? ». Aucune forge ne le fait vraiment aujourd'hui. Les outils proches (Backstage, Sourcegraph) sont séparés de la forge et demandent beaucoup de configuration.

## Pourquoi c'est fort

- **Deux idées en une** : elle combine la carte du savoir (qui connaît quoi) et la visite guidée du repo dans une seule vue concrète.
- **Deux publics** : le nouveau venu comprend le projet, l'équipe sait qui solliciter.
- **Détection largement automatique** : `Dockerfile`, `docker-compose`, `Cargo.toml`, `package.json`, dossiers de migrations, etc.

## Pièges à anticiper

- **Fiabilité** : la détection automatique se trompe souvent sur les monorepos. Un fichier de surcharge, `assay.toml` à la racine du repo, corrige les noms et les limites des blocs (voir « Le socle en détail »).
- **Fraîcheur** : la vue doit se recalculer à chaque push, sinon on cesse vite de lui faire confiance.
- **Perception du « qui connaît »** : elle peut être vécue comme de la surveillance ou du classement. La présenter comme « personnes à contacter », basée sur l'activité récente et les revues, jamais comme un score. Chacun peut se retirer ou se déclarer référent.

## Stratégie

Presque tout repose sur deux briques : savoir à quel bloc appartient chaque fichier, et connaître le graphe entre les blocs. Les fonctionnalités sont donc classées selon ce qui dépend de ces deux briques.

Le moteur de détection se construit d'abord comme une lib et une CLI indépendantes, qui tournent sur n'importe quel repo, même hébergé sur GitHub ou GitLab. On vérifie la qualité de la détection sur de vrais projets avant de l'intégrer à la forge. Il vit maintenant dans Forgust, comme une crate du workspace (`crates/forgust-assay`), et sa CLI tourne toujours sur n'importe quel repo.

## Le socle en détail : assay

Le moteur s'appelle **assay** (`crates/forgust-assay`) : une crate Rust, bibliothèque et CLI.
- `assay map <repo>` imprime la carte en JSON ; `--text` en donne un résumé lisible, pour vérifier la détection à l'œil sur un vrai repo.
- `assay view <repo> --open` en fait une page HTML, sans rien à charger, qu'on regarde dans le navigateur. Les blocs y sont des cartes, rangées de gauche à droite, de ce qui a besoin vers ce dont on a besoin. Un clic sur un bloc fait ressortir ses liens et montre ses langages, ses personnes à contacter, ses jobs et ce qui le déclare. C'est un premier jet de la Smart View, avant son intégration à la forge.
- En bibliothèque, `forgust_assay::map_snapshot` fait la carte d'un repo sans copie de travail, ce que garde une forge : on lui passe les chemins, le texte des fichiers que retient `forgust_assay::reads` (manifestes, CI, CODEOWNERS, `assay.toml`) et l'historique. C'est ainsi que Forgust l'appelle, à travers son stockage.

### Ce qu'est un bloc

Un bloc est une partie du repo qu'on construit, fait tourner ou déploie à part, et qu'une personne peut connaître. Son type (`kind`) dit ce qui l'a révélé, et ses racines (`roots`) sont les dossiers ou fichiers dont les fichiers sont les siens.

| Type | Révélé par | Racine |
| --- | --- | --- |
| `rust` | un `Cargo.toml` avec `[package]` | son dossier |
| `node` | un `package.json`, sauf la racine d'un workspace (npm, Yarn, pnpm, Lerna), qui ne fait que regrouper des paquets | son dossier |
| `python` | un `pyproject.toml` de projet (`[project]` ou Poetry), un `setup.py`, un `setup.cfg` | son dossier |
| `go` | un `go.mod` | son dossier |
| `migrations` | un dossier `migrations` (Rails : `db/migrate`) | ce dossier |
| `docker` | un `Dockerfile` (`Containerfile`, `x.Dockerfile`, `Dockerfile.x`) | le fichier et son `.dockerignore` |
| `service` | un service d'un fichier compose, hors piles de test (`tests/`, `e2e/`, `compose.test.yml`) | aucune : seul le compose le déclare |
| `ci` | `.gitlab-ci.yml`, `.github/workflows/` et les autres CI connues | ces fichiers |
| `docs` | le dossier `docs/` de la racine | ce dossier |
| `other` | déclaré dans `assay.toml` | ses `paths` |

- **Un fichier appartient au bloc dont la racine est la plus profonde** parmi celles qui le contiennent : les migrations de `forgust-db` vont au bloc du schéma, le reste de la crate à la crate. Un bloc imbriqué nomme son `parent`. Les fichiers hors de tout bloc (README, LICENSE, manifeste du workspace…) sont comptés à part.
- **Exclus par défaut** : les dossiers `fixtures`, `testdata`, `vendor`, `node_modules`, `third_party`, et le cache `.sqlx`. Ce sont des données de test qui ressemblent à de vrais projets, du code d'autres gens, ou des fichiers générés.
- **L'identifiant** (`id`) est le nom du paquet ou du service en minuscules et tirets (`forgust-server`, `db`). C'est `image` pour le Dockerfile de la racine et `<dossier parent>-migrations` pour un schéma. En cas de doublon, le type s'ajoute (`api-service` à côté de la crate `api`). Il reste stable tant que le nom ne change pas : c'est lui qu'utilisent `assay.toml` et la forge.
- **Le rôle** (`role`) est une supposition, qu'`assay.toml` corrige :
  - pour un paquet, d'après son manifeste : `web` (React, Vue, Svelte, Vite, Leptos…), `api` (axum, actix-web, express, fastapi, gin…), `cli` (un binaire sans serveur web : `src/main.rs`, `bin`, `[project.scripts]`, `main.go` ou `cmd/`), `library` (ce qu'un autre importe : un paquet Rust ou Go sans binaire, un paquet npm qui a un `main` ou des `exports`, un projet Python qui a un `[build-system]`). Sans indice, pas de rôle : un outil npm qui n'a que des scripts, une app Python ;
  - pour un service, d'après son image : `database`, `cache`, `queue`, `search`, `storage`, `mail`.
- **La description** (`description`) dit en une ligne à quoi sert le bloc : celle de son manifeste (`description` de Cargo, npm ou pyproject, le label `org.opencontainers.image.description` d'un Dockerfile), ou celle qu'`assay.toml` lui donne, par exemple à un service compose ou à un module Go, qui n'en ont pas.

### Les liens

Un lien va de celui qui a besoin vers celui dont il a besoin. Il porte le fichier qui le dit (`via`) et une certitude :
- `declared` : un manifeste ou `assay.toml` le dit ;
- `inferred` : c'est une supposition, à afficher à part et à confirmer. `assay.toml` la confirme en la déclarant avec son type (`kind = "connects"`) : elle devient `declared`.

| Type | Lu dans |
| --- | --- |
| `uses` | une dépendance Cargo par chemin ou de workspace (pas les dev-dependencies), une dépendance npm vers un paquet du repo |
| `builds` | les `COPY` et `ADD` d'un Dockerfile ; parmi les paquets Cargo copiés, seulement ceux que nomme son `cargo build -p` ou `--bin`, s'il en nomme : les autres ne sont là que pour que le workspace se résolve, ou comme dépendances, que `uses` montre déjà |
| `runs` | le `build` d'un service compose |
| `depends_on` | le `depends_on` d'un service compose |
| `connects` (supposé) | l'environnement d'un service qui nomme un autre service comme hôte : dans une adresse (`postgres://app@db:5432`, `host=db`), ou comme valeur d'une variable d'hôte (`REDIS_HOST=redis`) ; jamais comme valeur d'une autre variable (`POSTGRES_USER=forgust`) |
| `calls` | `assay.toml` : ce qu'aucun fichier ne dit, comme une app web qui appelle une API |

Les appels à l'exécution (Web → API, API → file de messages) ne se lisent pas de façon fiable dans le code. Ils viennent de l'environnement de compose (supposés) ou d'`assay.toml` (déclarés).

### Les personnes

- **Historique git** : les commits de l'année écoulée qui touchent le bloc, `.mailmap` appliqué, robots écartés (Dependabot, Renovate…). Deux adresses sous le même nom, ou deux noms sur la même adresse, sont une seule personne. Deux identités sans rien en commun ne se réunissent que par `.mailmap`.
- **Personnes déclarées** (`owners`) : les propriétaires CODEOWNERS d'au moins la moitié des fichiers du bloc, puis les référents d'`assay.toml`.
- **Bus factor** : le nombre de personnes qui ont fait au moins un cinquième des commits récents du bloc. À 1, une seule personne le connaît.
- Chacun peut se retirer (`[people] exclude`, par un de ses noms ou une de ses adresses : il disparaît sous toutes ses identités) ou se déclarer référent.
- Le nombre de commits ordonne les contacts. Il s'affiche à côté de chacun, avec sa dernière modification, pour dire d'où vient le contact : jamais comme un score ou un rang.

### Le lien job → bloc, sans toucher au format de CI

Forgust n'a pas de format de CI à lui : il lit `.gitlab-ci.yml`, un sous-ensemble de GitLab CI (ADR-0003), et refuse les mots-clés qu'il ne connaît pas. Une clé `block:` casserait donc la compatibilité avec GitLab. Le lien vient plutôt de deux sources :

1. **Les `rules: changes` des jobs** (ou `only: changes`), que Forgust gère déjà, y compris ceux qu'un job hérite par `extends:` : un job qui ne tourne que sur `crates/forgust-server/**` appartient au bloc `forgust-server`. Pour GitHub ou Forgejo, ce sont les filtres `paths` du workflow.
2. **`assay.toml`**, sinon : `jobs = ["rust-*", "web-build"]` sur le bloc, par nom ou par motif.

Les blocs restent définis à un seul endroit, et la CI reste telle quelle.

### `assay.toml`

À la racine du repo. Les blocs y sont désignés par les identifiants qu'imprime `assay map`. Une clé inconnue est une erreur, pas un réglage ignoré en silence.

```toml
version = 1
exclude = ["bench/", "ci-conformance/"]   # en plus des exclusions par défaut

[blocks.forgust-server]          # un bloc détecté, corrigé
name = "API"
description = "L'API, l'interface web et le SSH"   # à quoi il sert, en une ligne
role = "api"
referents = ["@alice"]           # à contacter, en plus de l'historique
jobs = ["rust-*"]                # ses jobs de CI, par nom ou motif

[blocks.forgust-diff]
merge_into = "forgust-server"    # une partie d'un autre bloc

[blocks.forgust-gen-api]
ignore = true                    # pas un bloc : ses fichiers vont au bloc autour

[blocks.site]                    # un bloc qu'aucun manifeste ne donne
kind = "docs"
paths = ["site/"]

[[links]]                        # ce qu'aucun fichier ne dit (kind = "calls" par défaut)
from = "forgust-web"
to = "forgust-server"

[[links]]                        # un lien trouvé, mais faux
from = "image"
to = "forgust-bench"
remove = true

[[links]]                        # une supposition confirmée : elle devient declared
from = "forgust"
to = "mail"
kind = "connects"

[people]
exclude = ["quelquun@example.com"]  # par un nom ou une adresse : la personne entière
days = 180                       # l'historique lu, un an par défaut
```

### La sortie JSON (schéma 1)

Un extrait de la carte de Forgust (adresses masquées) :

```json
{
  "schema": 1,
  "commit": "43b5c3e…",
  "blocks": [
    {
      "id": "forgust-db-migrations",
      "name": "crates/forgust-db/migrations",
      "kind": "migrations",
      "role": "database",
      "roots": ["crates/forgust-db/migrations"],
      "manifests": [],
      "parent": "forgust-db",
      "files": 23,
      "languages": [{ "name": "SQL", "files": 23 }],
      "people": [
        { "name": "d3vyce", "email": "…", "commits": 13, "last": "2026-11-21" },
        { "name": "BlaalV8", "email": "…", "commits": 2, "last": "2026-11-18" }
      ],
      "bus_factor": 1,
      "source": "detected"
    }
  ],
  "links": [
    { "from": "forgust-server", "to": "forgust-db", "kind": "uses", "certainty": "declared", "via": "crates/forgust-server/Cargo.toml" },
    { "from": "forgust", "to": "mail", "kind": "connects", "certainty": "inferred", "via": "compose.yml" }
  ],
  "unassigned": { "files": 138, "sample": [".cargo/config.toml", "…"] }
}
```

- Les champs vides (`jobs`, `owners`, `people`, `warnings`…) sont omis. `warnings` liste ce qui n'a pas pu être lu, par exemple un manifeste invalide, laissé de côté.
- `schema` augmente quand un champ change de sens ou disparaît ; un nouveau champ optionnel le garde.
- La bibliothèque donne aussi `block_of(chemin)`, le bloc d'un fichier : c'est la base de l'impact sur les MR.

### Premier essai : Forgust

`assay map ../forgust` prend 0,1 s et trouve 19 blocs : les 7 crates, l'app web et son outil de génération d'API, le schéma de `forgust-db`, les 2 images, les 6 services de compose et la CI. Les 14 liens trouvés sont tous justes :
- `forgust-server` utilise `forgust-db`, `forgust-storage`, `forgust-ci-config` et `forgust-reviewer` ;
- l'image construit `forgust-server` et `forgust-web`, celle du reviewer construit `forgust-reviewer` ;
- le service `forgust` lance l'image, dépend de `db` et `s3`, et joint `mail` (lien supposé, à cause de `smtp://mail:1025`).

Il reste 138 fichiers hors bloc : ceux de la racine, `.github` hors workflows, et les fixtures de `ci-conformance`.

Le bus factor de Forgust vaut 1 sur presque tous les blocs : Bilal et BlaalV8 sont deux identités git sans rien en commun. Avec un `.mailmap` qui les réunit (essayé), il passe à 2 sur `forgust-server`, `forgust-web` et `forgust-db`.

### Dans Forgust

La page **Smart View** d'un projet, sous Code (`/-/smart_view/<branche, tag ou commit>`), montre la carte d'une version du code. Branche `feat/smart-view` de Forgust :
- le serveur lit à travers son stockage la liste des fichiers, le texte de ceux que retient `forgust_assay::reads` (2 000 au plus, 1 Mo chacun) et l'historique des jours voulus (5 000 commits au plus, `.mailmap` du commit appliqué), puis appelle `forgust_assay::map_snapshot`. La carte est gardée en cache par commit ;
- les personnes sont reliées à leur compte par leur adresse vérifiée, et leurs adresses ne quittent pas le serveur ;
- chaque bloc montre l'état de ses jobs dans le dernier pipeline du commit, et un job cassé allume le bloc ;
- les racines, manifestes, fichiers qui disent un lien, comptes et jobs sont des liens ; un `assay.toml` illisible s'affiche en haut de la page avec son erreur ;
- sur téléphone, les blocs forment une liste : un graphe y serait surtout hors de l'écran.

### Pas encore fait

- Les `include:` de `.gitlab-ci.yml` ne sont pas suivis : leurs jobs se rattachent par `assay.toml`.
- Les dépendances internes des projets Python et Go ne sont pas lues : seuls les blocs le sont.
- Les référents ne tiennent compte ni de `git blame` (qui a écrit les lignes actuelles) ni des revues, que seule la forge connaît.
- Un service compose n'a pas de personnes, puisqu'il n'a pas de fichiers à lui.

## Priorité 1 — Le socle

À construire en premier, dans cet ordre. Si la détection est mauvaise, tout le reste devient faux : c'est là que l'effort doit aller.

1. Détection des blocs par les manifestes (Dockerfile, docker-compose, Cargo.toml, package.json, migrations…). **Fait** dans assay.
2. Fichier de surcharge `assay.toml`. **Fait**.
3. Graphe des flux entre blocs : qui appelle quoi (Web → API → BDD → file de messages). **Fait** pour ce que disent les manifestes et compose ; les appels à l'exécution se déclarent dans `assay.toml`.
4. Référents tirés de l'historique git (git blame plus tard) et des revues, avec un badge bus factor quand une seule personne connaît un bloc. **Fait** pour l'historique, CODEOWNERS et les référents déclarés ; les revues viendront de la forge.
5. Impact sur les PR : les blocs touchés s'allument et leurs référents sont proposés comme relecteurs. À faire dans Forgust, avec `block_of` d'assay.
6. Lien job → bloc dans la CI, sans changer son format, avec pipeline et statut par bloc (détail ci-dessous). **Fait** pour le statut : assay rattache les jobs aux blocs, et la Smart View de Forgust montre l'état de chacun dans le dernier pipeline du commit. Le pipeline complet par bloc (étapes, durée, blocs en attente) reste à faire.

L'impact sur les PR est la brique qui apporte de la valeur au quotidien, pas seulement à l'onboarding.

**Visualisation du pipeline par bloc** : grâce au lien job → bloc, chaque job sait à quel bloc il appartient. Dans la vue, chaque bloc affiche donc son propre pipeline : étapes, durée, logs du dernier run. Une étape cassée allume le bloc concerné, et les blocs qui en dépendent apparaissent en attente.

## Priorité 2 — Les gains rapides

Peu chers une fois le socle en place, parce qu'ils réutilisent la carte fichiers → blocs et le graphe.

- **Simulation d'impact** : on demande « si je change le schéma de la table `users`, qu'est-ce qui casse ? ». C'est le calcul de l'impact sur les PR, mais sans PR : la vue liste les blocs, tests, référents et repos touchés avant qu'une ligne soit écrite.
- **Règles entre blocs** : déclarer par exemple « le Web ne doit pas appeler la BDD directement », et la CI bloque une PR qui viole la règle. L'idée la plus sous-estimée, parce qu'elle garde la vue juste dans le temps.
- **Issues et PR classées par bloc** : chaque issue est rattachée à un ou plusieurs blocs, automatiquement ou à la main. On voit la dette et les bugs ouverts par zone du code.
- **Santé par bloc** : statut CI, couverture de tests, dépendances obsolètes, issues ouvertes. Deux onglets en plus :
  - **Coût** : temps de CI consommé, taille de l'image Docker, temps de build.
  - **Sécurité** : CVE des dépendances, secrets utilisés, surface exposée (ports, routes publiques).
- **Diff de la vue entre deux versions** : comparer l'architecture entre deux releases (bloc ajouté, dépendance apparue, bloc qui a beaucoup grossi). Un changelog d'architecture automatique.
- **CI générée** : la vue propose la CI de chaque bloc (`cargo build` et `cargo test` pour un bloc Rust, build d'image pour un bloc Docker, install, lint et build pour un bloc Web). Elle arrive en PR pour être relue, et seuls les jobs des blocs touchés par un push sont relancés.
- **Revue adaptée au risque** : chaque PR reçoit un niveau de risque calculé d'après les blocs touchés, la familiarité de l'auteur avec ces blocs, la couverture de tests et les incidents récents. PR risquée : deux relecteurs dont un référent. PR triviale : fusion automatique possible.
- **Page d'accueil du bloc** : les 3 ou 4 fichiers à lire en premier, ce qu'il faut pour le lancer (variables d'environnement, ports, secrets requis, commande de démarrage), et un parcours d'onboarding avec une première « good first issue » et le référent à contacter.
- **Mémoire du bloc** : les décisions et grosses PR qui l'ont façonné, résumées par IA, plus un README et des notes de décision. La forge signale une doc obsolète quand le code a beaucoup changé depuis sa dernière mise à jour.

## Priorité 3 — Les gros paris

Forte valeur, mais coûteux ou dépendants d'autres briques : à lancer une fois le socle solide.

- **Contrats entre blocs** : les interfaces (schéma d'API, schéma de BDD, événements) sont détectées et versionnées. Une PR qui casse un contrat montre les consommateurs touchés, même dans d'autres repos de l'org, et ouvre les PR d'adaptation chez eux. Énorme valeur, mais détecter les schémas dans tous les langages est difficile.
- **Relier le code à la prod** : chaque bloc affiche ce qui se passe réellement en production (erreurs, latence, logs), et même les lignes de code les plus exécutées. À faire via choplog, qui fournirait les logs filtrés par bloc : presque un projet à part.
- **Agent IA par bloc** : il connaît le code, l'historique, les décisions et les règles du bloc. Il relit les PR qui le touchent, répond aux questions dans un chat limité au bloc et signale les écarts. Pour un changement multi-blocs, les agents concernés comparent leurs contraintes avant la revue humaine. La revue IA générique devient banale : la différence viendra du contexte par bloc, fourni par le socle.
- **Blocs partagés entre repos** : un bloc peut être une lib utilisée par d'autres repos de l'org. La vue montre qui en dépend ailleurs.
- **Mode incident** : pendant une panne, on sélectionne le bloc en cause, et la vue montre le dernier déploiement, les PR récentes, les référents disponibles et les blocs impactés en aval. Dépend du lien avec la prod.
- **Environnements par bloc** : un bouton « lancer ce bloc » démarre un environnement éphémère avec juste ce bloc et ses dépendances (BDD seedée, API mockée). Toute une infra à construire, lourde pour une petite équipe.

## À repousser loin

- **Édition inverse** : on ajoute un bloc « worker » sur la vue et on le relie à la file de messages ; la forge génère une PR avec le dossier, le `Dockerfile`, la CI et l'entrée dans `blocks.toml`. Géniale en démo, mais peu utilisée au quotidien, et les templates par stack coûtent cher à maintenir.
