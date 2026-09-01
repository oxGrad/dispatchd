import 'recipes/cargo.just'
import 'recipes/release.just'
import 'recipes/testenv.just'

_default:
  @just --choose

release-major:
  just _release major

release-minor:
  just _release minor

release-patch:
  just _release patch
