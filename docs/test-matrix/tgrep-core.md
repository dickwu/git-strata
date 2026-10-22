# tgrep-core 测试矩阵（Task 3f）

来源：`/Volumes/GWD/opensource/tgrep`（d25ade6）的 `tgrep-core/src/**` 与 `tgrep-core/tests/*.rs`，共 635 个 `#[test]`（`src` 525、`tests` 110）。行由 `matrix-common.md` 的 awk 生成器产出（另加 `--sort path`，按路径排序，行集与未排序输出一致；`src` 与 `tests` 各跑一次），每个测试函数一行，`src` 的行在前、`tests` 的行在后。

- `src` 下逐条分类的文件共 161 条：`worktrees/tests.rs`（57）、`walker.rs`（41）、`git_index.rs`（20）、`gitignore.rs`（17）、`rooted/tests.rs`（15）、`generations/mod.rs`（11）。`hybrid.rs` 与 `generations/git.rs` 没有任何测试，不产生行；`HybridIndex` 的遮蔽语义由 `worktrees/tests.rs` 与 `tests/snapshot_consistency.rs` 的行承接。
- `src/managed/` 的 19 个文件、120 条一律 `不保留`、目标 `—`，理由固定为「git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖」。这些行的「原意」按文件主题与测试名归纳（未逐条精读），每个文件的首行写主题，其后写 `同上`。
- `src` 其余 14 个文件的 244 条一律 `依赖承担`、目标 `—`：`search` 经库调用索引的构建、读取与查询路径，这些测试随库运行。原意的写法同上。
- `tests/` 共 110 条（8 个文件）：`shared_worktrees.rs`（30）、`generations.rs`（25）、`snapshot_consistency.rs`（7）、`managed_storage_macos.rs`（1）逐条分类；`corrupt_index_reader.rs`（5）与 `case_insensitive_roundtrip.rs`（1）读过全部断言，标 `依赖承担`；`managed_lifecycle.rs`（38）与 `managed_catalog_windows.rs`（3）标 `不保留`，理由同 `src/managed/`，仅 Windows 编译的用例另行注明，其「原意」按测试名与断言信息归纳（未读完近 3700 行的函数体）。`managed_storage_macos.rs` 虽以 managed 命名，却不调用任何 tgrep API，是对 macOS 内容代数的直接探针，归 `apfs`。
- 判断口径：`依赖承担` 只给 git-strata 实际经库调用的代码路径（索引的构建、读取与查询，行为在 tgrep-core 内部、git-strata 原样使用）；tgrep 的设施 git-strata 不用（managed v2、`stale_reads`、`.ignore` 与 `p4ignore.ini`、Windows、非 git 树、CLI 选项、tgrep 自己的存储布局与跨进程锁）→ `不保留`；git-strata 须在自己的模块里验证 → `保留` 或 `改写`。

| 来源 | 原意 | 目标模块 | 状态 | 理由 |
| --- | --- | --- | --- | --- |
| tgrep-core/src/builder.rs::disk_preflight_checks_exact_strategy_and_headroom_boundaries | 构建前磁盘预检：按策略与余量判定边界，探测失败与溢出照实传出，元数据不可得时保守计费 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::disk_preflight_propagates_probe_failures_and_checked_overflow | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::disk_preflight_charges_unavailable_metadata_conservatively | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::output_failure_preserves_native_io_kind_and_cause_including_json | 输出失败的分类：保留原生 IO 类别与原因（含 JSON）、受管承诺与非 IO 错误；spill 创建、spill 段、spill 读与索引文件写出的失败都不可回退 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::output_failure_preserves_managed_commitment_and_non_io_errors | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::output_failure_distinguishes_destination_directory_from_input_root | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::spill_creation_output_failure_is_not_fallback_eligible | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::spill_segment_output_failure_is_not_fallback_eligible | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::spill_read_output_failure_is_not_fallback_eligible | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::index_file_output_failure_is_not_fallback_eligible | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::disk_preflight_rejects_rebuild_before_invalidating_the_existing_index | 预检失败不破坏现状：重建前拒绝则旧索引保持，首次构建被拒不留输出，各写路径保持暂存输出完好 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::disk_preflight_leaves_no_output_on_a_rejected_initial_build | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::disk_preflight_keeps_staged_outputs_intact_on_every_writer_path | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::raced_oversized_files_are_excluded_from_published_stamps | 发布戳的可信度：竞态中变超限的文件不入戳，缺失读版本不用当前元数据补盖，仅文本记录精确解码身份，直接重建先作废旧证据 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::publication_never_stamps_a_missing_read_version_from_current_metadata | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::full_builder_records_exact_decoded_identity_for_text_only | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::direct_rebuild_invalidates_old_evidence_before_core_writes | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::batches_are_bounded_by_cumulative_heap_bytes | 构建批次的内存与文件数上限：按堆字节累计，映射字节在途有界，按文件数分批，超预算或大小未知的文件独占一批 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::large_mapped_files_still_fill_a_batch | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::mapped_bytes_in_flight_stay_bounded | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::batches_are_still_bounded_by_file_count | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::a_file_larger_than_the_budget_gets_its_own_batch | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::a_file_whose_size_is_unknown_gets_its_own_batch | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::owned_read_rejects_files_beyond_the_batch_budget | 拥有式读取的预算：超批预算拒绝，映射前检查配置上限，增长超出预留时报告并重试，许可释放批容量，超大读独占预算并使回退串行 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::configured_limit_is_checked_before_mmap | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::owned_read_reports_growth_beyond_its_reservation | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::indexed_read_retries_growth_with_the_full_batch_reservation | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::owned_read_permit_releases_batch_capacity | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::oversized_owned_read_claims_the_budget_exclusively | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::owned_read_budget_serializes_fallbacks_that_exceed_the_remaining_capacity | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::indexed_read_race_withholds_version_without_changing_decoded_identity | 读取竞态下的版本证据：竞态读不授予版本但不改变解码身份，mtime 还原竞态不授予版本，提取后不以元数据替换，映射读后验证版本，无读前元数据不能事后获信 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::indexed_read_mtime_restore_race_withholds_version | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::indexed_version_is_not_replaced_by_metadata_after_extraction | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::mapped_indexed_read_verifies_version_after_extraction | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::indexed_read_without_pre_read_metadata_cannot_gain_trust_after_read | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::default_builder_constructs_snapshot_but_explicit_none_is_preserved | 构建器默认与成员：默认构造快照而显式 None 被保留，批次覆盖每个文件一次，空遍历没有批，external 为默认策略，默认含点文件并记录查询可见性，`.git` 元数据默认排除，不索引自身存储，存储不得覆盖源根，文件名旁路表含内容索引之外的路径 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::batches_cover_every_file_exactly_once | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::an_empty_walk_produces_no_batches | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::external_is_the_default_strategy | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::default_build_includes_dotfiles_and_records_query_visibility | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::git_metadata_is_excluded_unless_ignore_rules_are_disabled | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::default_build_never_indexes_its_custom_storage | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::storage_cannot_cover_the_source_root | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::filename_sidecar_captures_paths_outside_the_content_index | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::external_strategy_matches_in_memory_index_contents | 构建策略等价：external 与内存索引内容一致、可搜索、不留 spill 目录；基础往返；`no_ignore` 含 `p4ignore.ini` 命中的文件 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::external_strategy_index_is_searchable | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::external_strategy_leaves_no_spill_directory_behind | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::build_index_writes_readable_round_trip_index | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::build_index_no_ignore_includes_p4ignored_files | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::append_overlay_merges_new_files_into_complete_index | 追加 overlay 与增量：新文件并入完整索引或 external 索引；精确文件 delta 跳过不可读输入且不把二进制当不可读；流式 delta 叠加到已有索引并支持替换与删除 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::append_overlay_merges_onto_an_external_built_index | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::exact_file_delta_skips_and_reports_an_unreadable_input | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::exact_file_delta_does_not_report_binary_files_as_unreadable | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::exact_file_delta_streams_onto_an_existing_index | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::streamed_delta_can_replace_and_delete_reader_files | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::streamed_delta_supports_deletion_without_new_files | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::gitignore_gate_hint_fires_only_when_rules_are_silently_inert | `.gitignore` 闸门提示：规则因不在 git 仓库内而静默失效时给出提示，没有 `.gitignore` 时不提示 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/builder.rs::gitignore_gate_hint_is_silent_without_a_gitignore | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::parses_special_labels | 编码标签解析（特殊标签、WHATWG 标签、未知标签拒绝）与受控索引解码（保留边界与修复字节、协作取消） | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::controlled_index_decoding_preserves_boundaries_and_repair_bytes | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::controlled_index_decoding_cooperates_with_cancellation | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::parses_whatwg_labels | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::rejects_unknown_labels | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::auto_decodes_utf16le_bom | BOM 与模式：auto 识别 UTF-16 BOM 并剥掉 UTF-8 BOM，none 保留原字节，显式编码解码无 BOM 输入，BOM 优先于显式编码，latin1 往返，纯 UTF-8 在各模式下不变 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::auto_decodes_utf16be_bom | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::auto_strips_utf8_bom | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::none_keeps_raw_bytes_including_bom | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::explicit_encoding_decodes_bomless_input | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::bom_wins_over_explicit_encoding | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::latin1_round_trips_high_bytes | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::plain_utf8_is_unchanged_in_every_mode | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::invalid_utf8_is_lossy_not_dropped | 无效字节与索引、搜索的一致性：无效 UTF-8 有损但不丢，只有 auto 与索引一致，索引与搜索对各种无效形态的修复一致，`source_bytes` 只还原被修复的范围，有效 UTF-8 借用不拷贝，二进制原样，owned 与 borrowed 结果一致且复用缓冲 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::only_auto_matches_the_index | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::index_sees_the_same_repaired_bytes_a_search_does | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::index_and_search_repairs_agree_on_every_invalid_shape | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::source_bytes_restores_only_repaired_ranges | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::valid_utf8_is_still_borrowed_for_the_index | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::binary_files_are_left_alone_for_the_index | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::decoding_owned_bytes_matches_decoding_borrowed_ones | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/encoding.rs::owned_valid_utf8_reuses_the_buffer | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::display_io_includes_inner_message | `Error` 类型：Display 含内层信息、`source()` 返回内层错误、`From` 转换保留来源、满足 Send 与 Sync | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::display_json_includes_inner_message | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::display_index_not_found_includes_path | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::display_index_corrupted_includes_reason | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::display_regex_includes_message | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::display_server_includes_message | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::source_io_returns_inner_error | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::source_json_returns_inner_error | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::source_string_variants_return_none | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::from_io_error_wraps_and_preserves_source | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::from_serde_json_error_wraps_and_preserves_source | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::result_alias_compiles | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/error.rs::error_is_send_and_sync | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::spill_allocation_preserves_colliding_files_and_directories | 外部排序构建索引：spill 文件分配不冲突且 arena 不超预算，external 结果与内存构建一致（含多次 spill 与空输入），spill 目录用后清理，合并后的 posting 列表可读有序，varint 往返，合并预读受预算约束且最小缓冲下仍正确 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::concurrent_sorters_in_one_index_dir_do_not_share_segments | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::arena_allocation_never_exceeds_the_budget | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::external_matches_in_memory_without_spilling | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::external_matches_in_memory_across_many_spills | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::spilling_actually_happens_under_a_small_budget | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::spill_directory_is_removed_after_write | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::merged_posting_lists_are_readable_and_sorted | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::empty_input_writes_an_empty_index | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::varint_roundtrip_covers_boundaries | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::varint_roundtrip_survives_a_minimal_buffer | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::merge_read_ahead_is_bounded_by_the_budget_not_by_fan_in | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/external.rs::merge_is_correct_with_minimal_read_buffers | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::builtin_table_covers_ripgrep_type_count | ripgrep 风格的文件类型过滤：内置类型表、别名、glob 与大小写变体，否定优先于选择，多类型并集，空过滤器匹配全部，未知类型报错，type-add 与 type-clear 的定义管理，参数顺序与最终定义求值，glob 不跨路径分隔符 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::aliases_resolve_to_the_same_globs | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::character_class_globs_match | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::case_varied_makefile_globs_match | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::negation_takes_precedence_over_selection | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::negation_only_includes_everything_else | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::multiple_selected_types_union | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::empty_filter_matches_everything | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::unknown_type_is_an_error | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::type_add_defines_a_new_type | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::type_add_appends_to_an_existing_type | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::type_add_include_copies_other_types | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::type_add_include_rejects_unknown_types | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::type_add_rejects_malformed_specs | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::type_clear_removes_a_definition | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::later_selection_wins | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::negate_then_select_of_overlapping_types_keeps_shared_globs | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::definitions_apply_in_argument_order | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::selections_resolve_against_final_definitions | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::unordered_equivalence | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::globs_do_not_cross_path_separators | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/filetypes.rs::types_added_since_the_old_hardcoded_list | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/generations/mod.rs::publication_lock_is_exclusive_and_released_when_dropped | 仓库级发布锁互斥，drop 后释放，可再次获取 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：库内基线索引发布由文件锁（`publication.lock`）串行；git-strata 自己的 per-repo 写锁在 daemon 层（见 daemon.md），二者不重叠 |
| tgrep-core/src/generations/mod.rs::default_content_eligibility_keeps_the_legacy_profile_wire_shape | 默认 IndexingProfile 的 JSON 形状保持旧版（没有 eligibility 字段），ContentOnly 才写出；eligibility 属于 generation 身份（storage_name 不同） | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：search 只用默认 profile，落盘的 generation 键兼容性由库负责 |
| tgrep-core/src/generations/mod.rs::cold_load_does_not_block_unrelated_warm_cache_hit | 冷加载某个 generation 时不持有全局缓存互斥锁，无关的热缓存命中不被阻塞 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：库内缓存并发；多会话同时 attach 时受益 |
| tgrep-core/src/generations/mod.rs::concurrent_cold_loads_recheck_before_cache_insertion | 两个线程并发冷加载同一 generation 目录，插入缓存前复查，最终两者拿到同一个 `Arc` | search | 改写 | 对应 search.md §7 `base_index_shared_across_sessions_same_tree`：同一 `BaseGen.tree` 的多个会话（含并发 attach）共享同一份基线索引（`Arc::ptr_eq`），不各建一份 |
| tgrep-core/src/generations/mod.rs::invalid_staged_generation_is_never_published | 暂存的 generation 清单与实际成员不符时发布被拒绝：最终目录不存在、`list()` 为空、不留半成品 | deps | 改写 | 设计不变量 5/6：依赖集 generation 在 staging 校验失败（安装未完成、输入哈希与目录不符）时不 rename 封存、不登记，现场保留；基线 generation 的建立同理（session 的建基线步骤）。库内索引 generation 的同名校验由 tgrep-core 自测 |
| tgrep-core/src/generations/mod.rs::parallel_blob_extraction_builds_the_serial_generation_bytes | 并行批量提取 blob 构建出的 generation 与串行逐字节相同，且各类内容（二进制、超限、UTF-16、latin1、无效 UTF-8、空文件、重复内容）归类正确 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：同一 tree 在不同并发度下得到同一份索引字节，是「同 tree 共享基线索引」的前提 |
| tgrep-core/src/generations/mod.rs::managed_parallel_blob_extraction_builds_the_serial_generation_bytes | 受管（managed）构建路径与传统路径输出逐字节相同 | — | 不保留 | 实现细节：managed v2 的资源许可与策略构建路径；git-strata 不使用 managed 接口（见 `SHARED_WORKTREE_INDEXES.md`「Managed policy」） |
| tgrep-core/src/generations/mod.rs::retained_parallel_extractions_fall_back_to_serial_under_private_pressure | 私有内存预算紧张时并行提取退回串行且结果相同 | — | 不保留 | 实现细节：managed v2 的私有内存预算 |
| tgrep-core/src/generations/mod.rs::parallel_extraction_reports_the_serial_blob_work_limit | 超出单 blob 工作上限时串行与并行都报同一个 `blob-work-limit` 压力原因 | — | 不保留 | 实现细节：managed v2 的 blob 工作上限与压力原因码 |
| tgrep-core/src/generations/mod.rs::parallel_extraction_builds_an_empty_tree | 空 tree 构建出 0 条目的 generation，各并发度结果一致 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：search 对空仓库或空 tree 的基线可直接依赖 |
| tgrep-core/src/generations/mod.rs::cancellation_stops_parallel_extraction | 取消许可后并行提取停止 | — | 不保留 | 实现细节：managed v2 的 `WorkPermit` 取消机制 |
| tgrep-core/src/git_index.rs::metadata_paths_strip_only_git_line_endings | 读 `.git` 指针文件与 `commondir` 时只去掉 git 写入的行尾（LF 或 CRLF），保留路径两侧的空格；只有行尾则报错 | gitio | 保留 | 工作区的 `.git` 是指向主 checkout 公共目录的 gitfile（设计 §6.1 第 2 步、§6.8），解析必须与 git 一致；`gitio` 借用 `git_index.rs`，函数与断言原样迁移 |
| tgrep-core/src/git_index.rs::metadata_paths_preserve_native_bytes | 指针路径含非 UTF-8 字节时按原生字节保留（Unix） | gitio | 保留 | 同上；对应 `vendor/ignore/PATCHES.md` 的 Parser delta（gitfile/commondir 按字节读取）。APFS 本身不产生非 UTF-8 名，但函数按字节处理，迁移成本为零 |
| tgrep-core/src/git_index.rs::reads_a_version_2_index | 解析版本 2 的 `.git/index`，得到全部已跟踪路径，未跟踪路径不在其中 | gitio | 保留 | `read_index` 直接解析 index 而不起 git 子进程（设计 §5 `gitio` 行），是 `index_parse_tracked_never_ignored` 的前提 |
| tgrep-core/src/git_index.rs::matches_paths_without_regard_to_case_or_separator | 已跟踪集合的成员判定忽略大小写，并归一分隔符与前导 `/`（规则按小写写、文件系统返回另一种拼写时仍命中） | gitio | 改写 | `IndexInfo.tracked` 是区分大小写的 `BTreeSet<RepoPath>`，`ignorecase` 为真时用 `apfs::case_fold_eq` 比较；反斜杠与前导 `/` 的归一化是 Windows 路径形态，不保留 |
| tgrep-core/src/git_index.rs::finds_tracked_files_under_a_directory | 判断某目录之下是否有已跟踪文件（大小写不敏感）；前缀须止于路径分隔符，`qlogs` 不得命中 `qlogs_like/` | gitio | 改写 | 被 ignore 规则命中的目录若含已跟踪文件就不能剪枝；`gitio` 提供等价查询（API 换成 `IndexInfo` 的方法），分隔符边界断言原样保留 |
| tgrep-core/src/git_index.rs::handles_a_path_at_and_beyond_the_length_field_limit | v2 条目 12 位的长度字段在 0xFFF 处饱和，之后以 NUL 为界；0xFFE、0xFFF、0x1000、0x1100 长度的路径都能正确解析 | gitio | 保留 | 同 `reads_a_version_2_index`，解析器边界用例原样迁移 |
| tgrep-core/src/git_index.rs::rejects_input_that_is_not_an_index | 空输入、签名错误、不认识的版本一律拒绝，而不是猜 | gitio | 保留 | 与设计不变量 6 一致：不确定就失败，不凭空造条目 |
| tgrep-core/src/git_index.rs::refuses_a_truncated_index_instead_of_inventing_entries | 在头部、首个条目路径内、第二个条目路径内截断的 index 都被拒绝 | gitio | 保留 | 同上 |
| tgrep-core/src/git_index.rs::tolerates_the_trailing_data_a_real_index_carries | 条目之后的扩展区与尾部校验和不得导致拒绝（git 写出的真实 index 都带） | gitio | 保留 | 同 `reads_a_version_2_index` |
| tgrep-core/src/git_index.rs::a_count_larger_than_the_data_is_refused | 头部声明的条目数大于数据所能容纳时拒绝 | gitio | 保留 | 同上 |
| tgrep-core/src/git_index.rs::a_header_claiming_four_billion_entries_does_not_reserve_for_them | 头部声称 u32::MAX 个条目时不按此数预留内存（否则约 146GB 的分配会直接中止进程），返回拒绝 | gitio | 保留 | 对不可信输入的容量夹取：会话内的进程可以改写 index 文件，daemon 不能因此崩溃 |
| tgrep-core/src/git_index.rs::reads_a_version_4_index_with_prefix_compressed_paths | 版本 4 的前缀压缩路径只能顺序解码，逐条还原出完整路径 | gitio | 保留 | `feature.manyFiles` 等配置会把 index 升到 v4，用户的仓库可能是 v4 |
| tgrep-core/src/git_index.rs::reads_a_split_index_through_its_shared_half | 拆分 index（`sharedindex.<hash>`）：共享半加上链接扩展里的删除与替换位图，再并入拆分半的新增；无位图时不删不换 | gitio | 保留 | 同 `reads_a_version_2_index`；拆分 index 是 `core.splitIndex` 的产物 |
| tgrep-core/src/git_index.rs::a_split_index_without_a_matching_shared_half_is_refused | 缺少或不匹配共享半、位图越界、替换数超过条目数时拒绝；全零的基准无需共享半 | gitio | 保留 | 只读拆分半会少报已跟踪文件，宁可拒绝（不变量 6） |
| tgrep-core/src/git_index.rs::ewah_runs_and_literals_are_bounded_by_the_shared_index | EWAH 位图的游程与字面字不得越过共享 index 的条目数，标记声称的字面字数超出位图时拒绝 | gitio | 保留 | 同 `reads_a_version_2_index` |
| tgrep-core/src/git_index.rs::reads_the_split_indexes_git_writes | 用真实 git 生成 v2 与 v4 的拆分 index（含删除、替换、新增），解析结果与 `git ls-files -z` 逐项一致 | gitio | 保留 | 与真实 git 做差分比较，是 `read_index` 正确性的最强证据；测试需要 PATH 里有 git |
| tgrep-core/src/git_index.rs::reads_ignorecase_from_a_repository_config | 从仓库 config 读 `core.ignorecase`：节名与键名不分大小写，`true`、`True`、`yes` 为真，其他节里的同名键不算，缺省为假 | gitio | 保留 | 设计 §3「路径比较按仓库的 ignorecase」；这是 `IndexInfo.ignorecase` 的来源 |
| tgrep-core/src/git_index.rs::follows_a_gitdir_pointer_file | `.git` 为 `gitdir: <path>` 指针文件时，经它读到 config 与 index | gitio | 保留 | 会话工作区的 `.git` 是 gitfile（设计 §6.8） |
| tgrep-core/src/git_index.rs::linked_worktree_uses_the_common_config_and_its_own_index | 链接工作树的 config 取自公共目录（`extensions.worktreeConfig` 启用时 `config.worktree` 覆盖，未启用则不生效），index 取自自己的 git dir | gitio | 保留 | 同上；会话工作区与主 checkout 共用 object store 与 config，index 各自独立 |
| tgrep-core/src/git_index.rs::a_directory_without_git_reports_nothing | 目录里没有 `.git` 时 `ignorecase` 为假，也不返回已跟踪集合 | gitio | 改写 | `read_index` 对没有仓库的目录返回显式错误（`Result`）而不是空集；调用方不得把「无 index」当成「无已跟踪文件」 |
| tgrep-core/src/gitignore.rs::collects_a_gitignore_that_its_own_rules_ignore | `build/.gitignore` 内容为 `*` 时规则自身也被命中，但其规则仍须生效（`build/artifact.o` 被忽略） | gitio | 改写 | `is_ignored` 的规则来源与成员清单是两回事：漏掉这条会让监听与对账索引一棵遍历本会跳过的子树；建议与 `git check-ignore` 做差分核对 |
| tgrep-core/src/gitignore.rs::builds_matcher_from_root_gitignore | 根 `.gitignore` 的 `*.log`、`target/` 规则生效，其余路径（`src/main.rs`）不受影响 | gitio | 改写 | 基础 gitignore 匹配（文件模式与目录模式）；API 换成 `Git::is_ignored(worktree, path)` |
| tgrep-core/src/gitignore.rs::malformed_ignore_line_does_not_discard_valid_rules | 一条畸形规则行（`[`）不使同文件其余有效规则作废 | gitio | 改写 | 同 `builds_matcher_from_root_gitignore`；git 对坏行同样只跳过该行 |
| tgrep-core/src/gitignore.rs::builds_matcher_from_dot_ignore_without_a_git_repo | `.ignore` 文件在没有 `.git` 时也生效 | — | 不保留 | `.ignore` 是 ripgrep/tgrep 的约定而非 git 规则；git-strata 的「被忽略」只按 git 规则（`.gitignore`、`info/exclude`、`core.excludesFile`），否则 overlay 会漏掉 git 会提交的文件 |
| tgrep-core/src/gitignore.rs::gitignore_is_inert_without_a_git_dir | 没有 `.git` 时 `.gitignore` 不贡献规则（与遍历一致） | — | 不保留 | tgrep 的 git 闸门（`.gitignore` 仅在仓库内生效）；会话工作区恒是 git 仓库（`.git` gitfile，设计 §6.8），闸门恒开；非仓库目录由 `read_index` 报错处理（见 `a_directory_without_git_reports_nothing` 行） |
| tgrep-core/src/gitignore.rs::root_dot_ignore_overrides_root_gitignore | 同一根目录里 `.ignore` 的否定规则压过 `.gitignore` | — | 不保留 | `.ignore` 的优先级是 tgrep 的约定，非 git 规则 |
| tgrep-core/src/gitignore.rs::nested_dot_ignore_overrides_gitignore_in_the_same_directory | 同一嵌套目录里 `.ignore` 压过同目录 `.gitignore` | — | 不保留 | 同上 |
| tgrep-core/src/gitignore.rs::returns_none_when_no_rules | 没有任何规则文件时不构造匹配器（没有路径被忽略） | gitio | 改写 | `is_ignored` 在没有任何规则时对所有路径返回 false；`Option<Matcher>` 的 API 形状不迁移 |
| tgrep-core/src/gitignore.rs::the_repository_exclude_is_found_through_a_worktree_pointer | 链接工作树里 `.git` 指向其 git dir，再经 `commondir` 找到全仓库共享的 `info/exclude` 并生效；该文件被删后路径解析为 None，但候选路径仍可得 | gitio | 改写 | 会话工作区的 `.git` 同为指针，主 checkout 的 `info/exclude` 必须作用于会话工作区（设计 §6.8） |
| tgrep-core/src/gitignore.rs::a_relative_commondir_resolves_against_the_worktree_git_dir | `commondir` 的相对路径（git 实际写的 `../..`）相对于工作树 git dir 解析，而非相对工作树根 | gitio | 改写 | 同上 |
| tgrep-core/src/gitignore.rs::local_whitelist_overrides_global_ignore | 本地规则的否定（`!keep.log`）压过全局忽略（`*.log`） | gitio | 改写 | git 的优先级：目录内 `.gitignore` 高于 `info/exclude` 高于 `core.excludesFile`；`is_ignored` 须遵守 |
| tgrep-core/src/gitignore.rs::with_nested_keeps_deeper_rules_ahead_of_local_rules | 更深目录的规则排在上层规则之前：`pkg/` 内的 `!keep.log` 压过根的 `*.log` | gitio | 改写 | 同上（更深的目录优先） |
| tgrep-core/src/gitignore.rs::builds_matcher_from_windows_style_p4ignore | Perforce 的 `p4ignore.ini` 里 Windows 风格（反斜杠）的规则 | — | 不保留 | Perforce 约定，非 git；git-strata 不读 `p4ignore.ini` |
| tgrep-core/src/gitignore.rs::p4ignore_reinclude_keeps_parent_directory_walkable | `p4ignore.ini` 重新包含子项时其父目录仍可遍历 | — | 不保留 | 同上 |
| tgrep-core/src/gitignore.rs::nested_gitignore_rules_stay_scoped_to_their_directory | 嵌套 `.gitignore` 里的 `*`、`*.c` 只作用于它所在目录，不得提升成全树规则（Linux 内核树上曾使监听丢弃全部事件） | gitio | 改写 | `.gitignore` 规则相对自己所在目录生效，是 `is_ignored` 的基本契约 |
| tgrep-core/src/gitignore.rs::relative_gitignore_paths_trip_the_debug_guard | 向匹配器构造函数传相对路径会触发 debug 断言（`should_panic`） | — | 不保留 | 实现细节：tgrep 匹配器构造函数的 `debug_assert` 契约（要求绝对路径），仅 debug 构建；`Git::is_ignored` 接受 RepoPath，没有这类入口 |
| tgrep-core/src/gitignore.rs::deeper_gitignore_negation_overrides_a_shallower_ignore | 更深目录里的 `!important.log` 压过浅层目录的 `*.log` | gitio | 改写 | 同 `with_nested_keeps_deeper_rules_ahead_of_local_rules` |
| tgrep-core/src/live.rs::replacement_admission_reuses_the_removed_files_table_capacity | 内存 LiveIndex：替换准入复用被移除文件表的容量，删除保留存储计费直到重建，后代查找感知目录边界，清除已对账路径只移除这些路径的条目与墓碑 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/live.rs::deletion_retains_backing_storage_charges_until_rebuilt | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/live.rs::descendant_lookup_is_directory_boundary_aware | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/live.rs::clearing_reconciled_paths_removes_entries_and_tombstones_only_for_them | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/managed/accounting.rs::transactional_totals_count_shared_references_and_reservations_once | managed v2 记账：事务性总额对共享引用与预留只计一次，记账更新随源行回滚 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/accounting.rs::accounting_updates_rollback_with_their_source_rows | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/authentication_native.rs::writable_mapping_blocks_verification_after_creator_handle_closes | managed v2 原生认证：可写映射阻止验证，租约观察非阻塞写者的中断，macOS 原生证据括住页且自身 I/O 之后不复用 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/authentication_native.rs::live_lease_observes_nonblocking_writer_break_and_recovers_after_close | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/authentication_native.rs::macos_native_evidence_brackets_pages_and_is_not_reused_after_own_io | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::unreserved_verifier_admission_does_not_hold_the_catalog_before_memory | managed v2 目录库：准入与认证边界、策略重放回执、真实的 CAS 失败、提交边界错误、WAL checkpoint 争用、SQLite POSIX 锁、版本与操作重放的权威性、回滚与已提交策略的区分、页耗尽回滚 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::independent_authentication_boundary_rejects_missing_zero_and_future_formats | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::independent_concurrent_policy_replay_preserves_original_receipt | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::independent_queued_policy_replay_ignores_obsolete_headroom | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::independent_distinct_policy_operations_preserve_real_cas_failure | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::independent_metadata_execution_reports_real_commit_boundary_errors | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::wal_checkpoint_contention_precedes_mutation_not_a_successful_commit | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::catalog_probes_preserve_sqlite_posix_locks | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::catalog_owner_survives_without_a_repository | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::catalog_versions_and_operation_replay_are_authoritative | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::catalog_failures_distinguish_rollback_from_committed_policy | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::policy_reduction_reserves_its_own_publication_and_rejection_does_not_trap_updates | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::database_page_exhaustion_rolls_back_without_spilling_unbounded_wal | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog.rs::catalog_pages_preserve_old_object_versions | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog_io.rs::full_sync_without_powersafe_overwrite_pads_beyond_the_database_page_count | managed v2 目录库（SQLite）I/O：无 powersafe overwrite 时 full sync 填充到页数之外，写入器禁用自动 checkpoint、spill 与未计账 mmap，发布边界含数据库增长与 shm 区 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog_io.rs::all_managed_writers_disable_auto_checkpoints_spill_and_unaccounted_mmap | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/catalog_io.rs::publication_bound_includes_database_growth_and_prospective_shm_regions | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/clock.rs::age_requires_proven_monotonic_progress_and_unchanged_use | managed v2 时钟：年龄须由已证明的单调进展与未变的使用得出，原生启动时钟可识别且单调 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/clock.rs::native_boot_clock_is_identified_and_monotonic | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::timestamp_drift_is_not_authority_and_original_payload_still_collects | managed v2 回收（collection）：时间戳漂移不作授权，竞争清理保持可恢复，验证重启与缓存证明，外来读者打断租约则失败关闭，发布认证，LRU 遍历，待处理意图与回滚进度 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::competing_control_cleanup_keeps_the_selected_object_pending_and_resumable | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::discarded_verification_restarts_before_accepting_a_changed_earlier_prefix | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::independent_read_only_inventory_preserves_verification_progress | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::policy_and_allocation_versions_restart_cached_prefix_authentication | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::expiration_releases_cached_native_resources_and_restarts_at_the_first_byte | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::idle_shutdown_discards_cached_proof_but_an_active_verifier_keeps_admission_open | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::busy_idle_probe_preserves_cached_proof_for_external_work | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::foreign_reader_breaks_the_cached_lease_and_collection_fails_closed | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::foreign_reader_at_lease_acquisition_keeps_collection_busy_and_retryable | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::busy_idle_probes_preserve_staging_reclamation_and_storage_admission | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::cancellation_at_a_verified_page_discards_all_cached_proof_without_delete_credit | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::missing_or_corrupt_producer_rows_never_authorize_destructive_io | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::ownership_mutation_at_verified_page_and_final_validation_barriers_has_zero_credit | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::payload_rewrite_with_all_basic_timestamps_restored_is_not_authenticated | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::macos_detects_late_prefix_writes_before_validation_and_after_own_io | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::benign_owned_ea_changes_require_content_authentication_without_an_ea_allowlist | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::publication_authenticates_empty_and_multi_page_members_without_reopening_producers | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::publication_preserves_replaced_or_ownership_changed_members | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::publication_requires_complete_unwithdrawn_producer_inventory | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::publication_holds_native_protection_through_inventory_commit | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::publication_does_not_bypass_retained_writable_mappings | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::single_page_passes_resume_member_deletion_without_spending_the_page_on_a_cached_id | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::collection_requires_new_proven_grace_after_clock_rollback_restart_or_missing_age | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::lru_traversal_excludes_new_and_moving_objects_without_losing_its_selected_order | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::pending_intent_preserves_unexpected_mutations_and_recovers_only_its_exact_truncation | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/collection.rs::collection_progress_is_not_published_from_a_rolled_back_transaction | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/diagnostics.rs::observations_separate_partial_failures_cancellation_and_success_from_reclaimed_totals | managed v2 诊断：观测区分部分失败、取消与成功及已回收总量，回执重放不凭空增加维护轮次 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/diagnostics.rs::receipt_replays_do_not_invent_additional_maintenance_passes | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::history_pruning_preserves_cursor_versions_then_compacts_collected_incarnations | managed v2 清理与回收：历史修剪、控制文件清理与恢复、元数据压力下的安全清理、被修改成员与外部硬链接的保留、并发清理者互不破坏 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::owner_control_cleanup_requires_ended_handles_and_never_replays_bootstrap_as_new | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::metadata_pressure_blocks_new_work_but_not_reference_safe_cleanup | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::collection_preserves_modified_members_and_external_hard_links | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::owner_control_unlink_intents_recover_once_at_every_boundary | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::completed_concurrent_control_removal_does_not_repeat_credit | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::independent_control_cleanup_preserves_unrelated_owner_admission | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::independent_concurrent_cleaner_preserves_active_control_verification | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::control_access_blocks_idle_stop_and_protects_probes | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::independent_control_permission_changes_are_preserved | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::independent_control_ownership_change_after_intent_is_preserved | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/housekeeping.rs::missing_retired_root_guard_does_not_block_cleanup_or_alias_a_new_root_guard | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/inputs.rs::one_pass_freezes_present_and_absent_ignore_sources | managed v2 受限输入读取：一次遍历冻结存在与缺失的忽略来源，模式准入有界，元数据读取保留限额与取消类别，分块检查取消，非常规的忽略输入（FIFO）不阻塞 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/inputs.rs::pattern_admission_is_bounded_and_preserves_resource_errors | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/inputs.rs::metadata_reader_preserves_limit_and_cancellation_categories | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/inputs.rs::bounded_reader_checks_cancellation_after_each_input_chunk | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/inputs.rs::nonregular_ignore_inputs_cannot_block_on_fifo_open | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/inventory.rs::inventory_is_paged_read_only_and_does_not_enter_unknown_directories | managed v2 盘点：分页只读且不进入未知目录，与游标共享准入并阻止原子空闲关停，独立 owner 与外来条目被保留 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/inventory.rs::inventory_shares_cursor_admission_and_blocks_atomic_idle_shutdown | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/inventory.rs::discovery_reports_independent_owners_and_preserves_foreign_entries | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/lifetime.rs::positive_owner_proof_requires_the_same_lifetime_guard | managed v2 所有者生命期：正向所有者证明须持有同一个生命期守卫 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/maintenance.rs::independent_concurrent_recovery_preserves_terminal_receipt_and_defers_new_work | managed v2 维护：并发恢复保留终态回执并延后新工作，空闲准入争用保留同令牌重放，并发空闲重放保留首个终态决定 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/maintenance.rs::concurrent_recovery_preserves_failed_and_cancelled_receipts | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/maintenance.rs::independent_idle_admission_contention_preserves_same_token_replay | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/maintenance.rs::concurrent_idle_replay_preserves_the_first_terminal_decision | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/mod.rs::identities_are_not_paths_or_lossy_aliases | managed v2 标识与度量：标识不是路径也不是有损别名，未知度量不能序列化为零，只有 store 与 namespace 路径被保留 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/mod.rs::unknown_measurement_cannot_be_serialized_as_zero | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/mod.rs::only_store_and_namespace_paths_are_reserved_from_unguarded_access | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/policy.rs::required_limits_and_modes_fail_closed | managed v2 策略：必需的限额与模式缺失时失败关闭，配额含并行工作与分块上限 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/policy.rs::quotas_include_parallel_work_and_chunk_bounds | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/private_control.rs::private_control_roundtrip_rotation_and_size_bound | managed v2 私有控制文件：往返、轮换与大小上限，拒绝组或其他用户的访问与链接，原生访问检查拒绝受限主体，拒绝空 DACL | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/private_control.rs::private_control_rejects_group_or_other_access_and_links | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/private_control.rs::private_control_native_access_check_denies_restricted_principals | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/private_control.rs::private_control_rejects_a_null_dacl | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/process.rs::owned_process_helper | managed v2 子进程管理：已退出的组长被回收且不丢后代清理，取消时释放真实后代的 OS 生命期守卫，子进程输出上限在累积前检查 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/process.rs::exited_leaders_are_reaped_without_losing_live_descendant_cleanup | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/process.rs::cancelled_process_releases_a_real_descendants_os_lifetime_guard | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/process.rs::child_output_limit_is_checked_before_accumulating_unbounded_data | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/roots.rs::root_probe_observes_concurrent_completed_retirement | managed v2 根守卫与退役：并发完成的退役可被观测，已退役根的探测不破坏活动的验证者，缺少退役证据的守卫保持未知 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/roots.rs::root_retirement_observes_concurrent_completed_retirement | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/roots.rs::retired_root_probe_does_not_break_an_active_control_verifier | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/roots.rs::missing_guard_without_exact_retirement_evidence_stays_unknown | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/storage.rs::macos_native_ownership_distinguishes_absent_and_changed_extended_acl | managed v2 存储：macOS 原生属主与扩展 ACL 的缺失和变化可区分，无打开观察保持身份、大小与分配，只删除预期归属的文件，已有子项不被静默替换 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/storage.rs::no_open_observation_preserves_identity_size_and_allocation | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/storage.rs::storage_only_removes_the_expected_owned_file | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/storage.rs::existing_child_is_not_silently_replaced | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::read_limits_are_admitted_once_and_version_changes_stop_work | managed v2 工作许可与计量：读限额一次准入，存储准入回收 WAL，worker、读字节与父目录句柄共享且取消不泄漏，哈希表增长计费，认证证明按有界批哈希全部字节，发布认证遵守预算，存活的写者阻止发布 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::control_refresh_observes_independent_commits_and_owner_guard_closure | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::storage_admission_reclaims_catalog_wal_before_refusing | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::storage_admission_samples_catalog_files_after_the_writer_checkpoint | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::view_admission_rejects_a_superseded_policy_snapshot | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::workers_and_read_bytes_are_shared_and_waiters_cancel_without_leaks | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::parent_handles_are_shared_and_cancelled_waiters_do_not_leak | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::hash_table_growth_charges_reallocation_and_releases_unused_headroom | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::hash_table_rehash_keeps_its_existing_backing_charge_after_deletions | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::read_admission_waits_for_private_buffers_but_not_retained_memory | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::oversized_read_claims_the_whole_shared_gate_and_releases_actual_charge | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::simultaneous_operations_share_input_capacity_and_release_waiters | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::intended_proof_hashes_every_accepted_byte_across_bounded_batches | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::intended_proof_does_not_adopt_changed_output_before_producer_close | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::publication_authentication_keeps_the_admitted_deadline_after_policy_reduction | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::closed_publication_authentication_obeys_the_existing_work_budget | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::intended_proof_retries_exact_rows_after_postcommit_response_loss | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::intended_proof_completion_crashes_never_invent_missing_content_evidence | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::live_writer_blocks_publication_even_after_sync_until_its_handle_is_closed | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::failed_member_creation_keeps_unknown_files_and_releases_only_ended_work | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/managed/work.rs::producer_recovery_quarantines_modified_closed_output_without_reinventoring_it | 同上 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/src/meta.rs::filestamps_reject_paths_outside_the_root | 索引元数据与文件证据：stamp 拒绝根外路径，旧元数据不证明隐藏覆盖，证据兼容旧格式并往返 content id，畸形 content id 只丢该路径的身份 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/meta.rs::legacy_metadata_does_not_prove_hidden_coverage | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/meta.rs::file_evidence_is_legacy_compatible_and_roundtrips_ids | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/meta.rs::malformed_content_id_drops_only_that_paths_identity | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/meta.rs::file_version_distinguishes_subsecond_modified_times | 文件版本（`file_version`）：区分亚秒级 mtime，精确版本往返且不改变旧 stamp，畸形或不兼容的版本逐路径 fail-open，旧式插入与证据生命周期会清除版本，未受信或不匹配的版本不持久化，纪元前时间往返，无效的 unix ctime 纳秒不被信任 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/meta.rs::precise_versions_roundtrip_without_changing_legacy_stamps | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/meta.rs::malformed_or_incompatible_versions_fail_open_per_path | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/meta.rs::legacy_insert_and_evidence_lifecycle_clear_versions | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/meta.rs::untrusted_or_mismatched_versions_are_not_persisted | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/meta.rs::version_timestamps_roundtrip_before_the_epoch | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/meta.rs::invalid_unix_change_nanoseconds_are_not_trusted | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::file_tables_reject_unsafe_paths_in_every_format | 磁盘表的安全与版本：文件表在每种格式下都拒绝不安全路径，版本化文件表拒绝旧读取器 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::versioned_file_table_rejects_legacy_readers | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_lookup_roundtrip | 磁盘格式往返与边界：lookup、file entry、posting entry 往返，超长路径拒绝与最大长度接受，截断的头与路径被拒，空表可读，分块编码保持，lookup 在容量处 flush | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_file_entry_roundtrip | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_write_file_entry_roundtrip | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_encode_file_entry_rejects_oversized_path | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_encode_file_entry_accepts_max_path | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_decode_file_entries_rejects_truncated_header | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_decode_file_entries_rejects_truncated_path | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_decode_file_entries_empty_ok | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_posting_entry_roundtrip | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_write_posting_entries_preserves_chunked_encoding | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_write_lookup_entries_flushes_at_capacity | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/ondisk.rs::test_buffered_writers_preserve_scratch_on_io_error | 缓冲写入器遇到 IO 错误时保留 scratch 文件 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/path_index.rs::filename_sidecar_rejects_unsafe_paths | 文件名旁路索引（filename sidecar）：拒绝不安全路径，缺失即旧索引，空与非空路径集往返，可见性与文件名成员一并往返，拒绝截断与尾随数据 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/path_index.rs::missing_sidecar_identifies_a_legacy_index | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/path_index.rs::round_trips_empty_and_nonempty_path_sets | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/path_index.rs::visibility_and_filename_membership_round_trip_together | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/path_index.rs::rejects_truncated_and_trailing_data | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::negative_lookbehind_is_dropped_but_its_literal_survives | 正则放宽：lookaround 被丢弃（含嵌套与各种形态）而字面量保留，字符类与大括号边界、命名组不误判，原子组变普通组，无法放宽的构造（verbose 模式、占有量词等）放弃，放宽不收窄语言，多模式放宽降级而不报错 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::every_lookaround_flavour_is_dropped | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::nested_groups_inside_a_lookaround_are_skipped_wholesale | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::a_leading_bracket_is_a_class_member_not_a_terminator | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::character_classes_nest | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::a_brace_is_only_a_quantifier_when_it_really_is_one | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::named_groups_are_not_mistaken_for_lookbehind | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::atomic_groups_become_ordinary_ones | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::a_plain_pattern_is_returned_unchanged | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::verbose_mode_and_comments_bail_out | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::constructs_that_cannot_be_widened_bail_out | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::possessive_quantifiers_bail_rather_than_guess | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::relaxed_multi_pattern_plan_degrades_instead_of_erroring | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::a_lookaround_only_pattern_falls_back_to_a_full_scan | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::a_dropped_lookaround_cannot_fuse_its_neighbours | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::relaxation_never_narrows_the_language | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_literal_plan | 查询计划与集合运算：字面、交替、短模式与通配计划，有序交并与稀疏合并，去重，OR 执行返回有序唯一的候选 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_alternation_plan | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_short_pattern | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_wildcard_is_match_all | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_intersect_sorted | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_union_sorted | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_union_many_sorted_handles_disjoint_lists_without_repeated_linear_growth | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_or_execution_returns_sorted_unique_candidates | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_sort_dedup_postings_keeps_sorted_unique_list_unchanged | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_sort_dedup_postings_sorts_and_dedups_unsorted_list | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_case_insensitive_literal_plan | 大小写不敏感计划：字面（含长字面与 Unicode）、正则、内联标志，模糊字符类与可选段不要求三元组，端到端 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::case_insensitive_literal_plan_handles_long_literals | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::case_insensitive_literals_preserve_unicode_candidates | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_case_insensitive_regex_plan | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::inline_case_flags_narrow_candidates_without_losing_matches | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::ambiguous_classes_and_optional_runs_do_not_require_trigrams | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_case_insensitive_end_to_end | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_mask_filtering_finds_match | mask 过滤：找到匹配并拒绝假阳性 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::test_mask_filtering_rejects_false_positive | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::multi_pattern_plan_unions_every_pattern | 多模式计划：取并集，合并为单分支，吸收不可索引的模式，支持固定字符串 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::multi_pattern_plan_collapses_to_single_branch | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::multi_pattern_plan_absorbs_unindexable_patterns | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/query.rs::multi_pattern_plan_supports_fixed_strings | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::open_rejects_lookup_with_non_multiple_size | 索引读取器打开时的结构校验：lookup 大小不是条目大小的倍数、空索引可打开、文件 id 越界或重复被拒、稠密文件表可打开 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::open_accepts_empty_index | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::open_rejects_files_with_out_of_range_id | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::open_rejects_files_with_duplicate_id | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::open_accepts_dense_files_table | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::descendant_lookup_is_sorted_and_directory_boundary_aware | 后代路径查找有序且感知目录边界 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::is_degenerate_detects_mmap_counter_disagreement | 退化索引判定：映射区与计数器不一致判为退化，空索引不算退化 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::is_degenerate_false_for_empty_index | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::validate_lookup_accepts_sorted_table | lookup 表校验与越界保护：排序、越界 postings、空表、偏移溢出；读取按 postings 实长夹取，越界返回空，部分尾项返回空，良构范围全部返回 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::validate_lookup_rejects_unsorted_table | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::validate_lookup_rejects_out_of_bounds_postings | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::validate_lookup_empty_is_ok | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::lookup_survives_offset_that_overflows_the_bounds_check | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::lookup_bounds_work_by_postings_len_not_declared_length | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::lookup_with_offset_past_end_returns_empty | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::lookup_clamps_length_to_the_entries_that_fit | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::lookup_with_offset_inside_a_partial_trailing_entry_returns_empty | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::lookup_still_returns_every_entry_of_a_well_formed_range | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/reader.rs::validate_lookup_reports_offset_that_overflows_the_range_math | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/rooted/tests.rs::regular_opens_and_literal_path_validation | 在根句柄之下按规范化的根相对路径打开常规文件；空串、`.`、`./dir/file`、`../file`、`dir/../file`、目录本身、绝对路径一律拒绝（Windows 的流与盘符形态另有拒绝） | apfs | 改写 | 读工作区文件的原语只认规范化的 RepoPath，经根句柄逐级打开，非常规文件与越界路径报错；Windows 形态不适用 macOS。⚠ 这是 apfs 新增的安全打开原语，Task 4 的 Produces 清单未列出，需 apfs.md 补上或由 T15 改标 |
| tgrep-core/src/rooted/tests.rs::query_sibling_batches_preserve_order_and_bound_pin_lifetimes | 同父目录的候选路径按每批至多 64 个分组，保持原序且不丢项，空输入没有批 | — | 不保留 | 实现细节：`sibling_batches` 只被 tgrep-cli 的服务端（`serve/shared/server.rs`）用来批量验证候选；git-strata 只链接 tgrep-core，不走该路径 |
| tgrep-core/src/rooted/tests.rs::query_sibling_reader_retains_only_its_scoped_parent_pins | `read_batch(parent)` 只能打开该父目录之下的文件，其余相对路径被拒；`finish()` 之后父目录才可改名（Windows 上持有期间不可改名） | — | 不保留 | 实现细节：`read_batch` 只是 `batch()` 的公开包装，库内外都没有调用方（只有这些测试在用）；WorktreeView 直接走 `batch()`（见 `sibling_batches_release_pins_and_recheck_ancestor_identity` 行）；Windows 的句柄持有语义不适用 |
| tgrep-core/src/rooted/tests.rs::owned_query_batch_retains_root_until_verified_finish | `owned_batch` 持有根目录的 `Arc`，直到 `finish()` 校验完成后才释放根 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：`owned_batch` 是 WorktreeSnapshot 候选批使用的库内生命周期，search 不直接操作 |
| tgrep-core/src/rooted/tests.rs::query_batch_finish_rejects_a_replaced_parent_without_following_it | 批次读取锚定在打开时的目录：父目录被改名并换成同名新目录后，批内仍读到原文件，`finish()` 报错（检出替换，不跟随） | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：WorktreeView 发布前的父目录身份复核依赖它 |
| tgrep-core/src/rooted/tests.rs::streaming_file_open_does_not_reuse_a_replaced_parent_handle | 非批次的 `open_file` 每次从根重新解析：父目录被换成同名新目录后读到新目录的内容，不复用旧句柄 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：库内 `RootedDir::open_file` 的路径解析语义；整体换目录对视图的影响见 `snapshots_reject_root_replacement_before_and_during_empty_queries` 行 |
| tgrep-core/src/rooted/tests.rs::sibling_batches_release_pins_and_recheck_ancestor_identity | 批次 `finish()` 返回祖先观察记录，`check_current_ids` 能检出之后发生的祖先替换；批外路径与 `..` 被拒 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：WorktreeView 对账发布前的祖先复核依赖它 |
| tgrep-core/src/rooted/tests.rs::ancestor_observations_are_point_in_time_not_lifetime_pins | 祖先观察只是时间点证据而非长期钉住：目录被换走再换回，ID 相等即通过，但不证明期间路径连续 | — | 依赖承担 | 同上 |
| tgrep-core/src/rooted/tests.rs::completed_batch_never_accepts_a_reparse_replacement | 批次完成后祖先被换成指向别处的目录链接（Windows 为 reparse/junction，Unix 为符号链接）时，重新开批与 `check_current_ids` 都失败 | — | 依赖承担 | 同上 |
| tgrep-core/src/rooted/tests.rs::batch_child_fifo_open_is_nonblocking_and_never_follows_symlinks | 批内子项为 FIFO 时打开立即报错而不阻塞，子项为符号链接时拒绝跟随（子进程加 20 秒截止，防止测试挂死） | apfs | 改写 | 同 `regular_opens_and_literal_path_validation` 的安全打开原语：FIFO 不阻塞、符号链接不跟随；我们的测试同样须带超时保护 |
| tgrep-core/src/rooted/tests.rs::directory_swap_before_open_rejects_outside_and_inside_links | 打开前中间目录被换成符号链接（指向根外或根内）时打开被拒，不跟随 | apfs | 改写 | checkpoint 读文件须经根句柄逐级 `openat(O_NOFOLLOW)`，目录被换成链接时拒绝，避免把工作区外的内容（如 `.env`）收进 blob；与 `clonefile` 的 `CLONE_NOFOLLOW` 配套 |
| tgrep-core/src/rooted/tests.rs::directory_swap_after_open_cannot_redirect_parent_handle | 已打开的父目录句柄不会因之后目录被换成链接而被重定向：仍读到原内容，之后的新打开才失败 | apfs | 改写 | 同 `directory_swap_before_open_rejects_outside_and_inside_links` |
| tgrep-core/src/rooted/tests.rs::pinned_root_cannot_be_retargeted_between_verification_and_open | 根目录在校验与打开之间被换成链接时，已钉住的根句柄仍读到原内容，且 `verify_root()` 报根身份已变 | apfs | 改写 | 同上；根身份复核还对应工作区目录被 rename 交换（设计 §6.4）之后须重新解析、不继续读旧目录 |
| tgrep-core/src/rooted/tests.rs::final_symlink_swap_is_rejected | 目标文件在打开前被换成符号链接时打开失败（不跟随到链接目标） | apfs | 改写 | 同上：最后一级用 `O_NOFOLLOW`，符号链接不读取其目标内容 |
| tgrep-core/src/rooted/tests.rs::fifo_swap_before_final_open_is_nonblocking | 目标文件在打开前被换成 FIFO 时返回 InvalidInput，不阻塞 | apfs | 改写 | 同上：以 `O_NONBLOCK` 打开并用 `fstat` 确认是常规文件 |
| tgrep-core/src/shared.rs::relative_checkpoint_destination_is_pinned_to_its_canonical_parent | 共享基线（SharedBase）的 checkpoint 目标防护：相对目标钉到规范父目录、必须带文件名，被改名或替换的目录不被旧路径覆盖，发布期间目录及祖先不可改名，拒绝被改名的基线及其后代，非 Unicode 的 Unix 根保持精确字节，父目录别名被改指或被换成指向基线的符号链接都不会把 checkpoint 写进基线 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/shared.rs::checkpoint_destination_requires_a_filename | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/shared.rs::renamed_checkpoint_directory_is_not_replaced_by_its_old_path | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/shared.rs::checkpoint_directory_and_ancestors_cannot_be_renamed_during_publication | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/shared.rs::checkpoint_rejects_a_renamed_base_and_its_descendants | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/shared.rs::non_unicode_unix_root_encoding_preserves_exact_bytes | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/shared.rs::retargeting_parent_alias_does_not_redirect_checkpoint_into_base | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/shared.rs::replacing_canonical_parent_with_base_symlink_does_not_redirect_publication | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::controlled_extraction_preserves_masks_and_checks_bounded_batches | 三元组提取与 mask：受控提取保持 mask 并检查有界批次，哈希打包，基本提取、短输入与去重，二进制判定，合并 mask 与小写拷贝语义等价，两种大小写都覆盖，next 与 loc mask 过滤假阳性 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_hash_packing | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_extract_basic | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_extract_short | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_extract_dedup | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_is_binary | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::merged_masks_match_a_materialised_lowercase_pass | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::merged_masks_cover_both_cases_of_a_trigram | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_extract_with_masks_basic | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_extract_with_masks_short | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_next_mask_filters_false_positive | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_loc_mask_nonzero | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_extract_merged_masks_ors_across_cases | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_extract_merged_masks_all_lowercase_no_dup | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/trigram.rs::test_extract_merged_masks_matches_lowercase_copy_semantics | 同上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/visibility.rs::explicit_hidden_root_does_not_hide_visible_descendants | 显式指定的隐藏根不隐藏其可见的后代 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖 |
| tgrep-core/src/walker.rs::index_keys_never_rename_a_native_name | 原生文件名转索引键时绝不改名：含字面 `\` 或非 UTF-8 的名字没有键（None），显示用的有损形式不当键；同目录里 `a/b.txt` 与名为 `a\b.txt` 的文件只保留前者的键 | types | 改写 | `RepoPath` 由原生名构造时不得把一个名字悄悄变成另一个（`\` 当分隔符、U+FFFD 替换）；无法忠实表示的名字显式拒绝并标出。macOS 例外：APFS 对 NFC/NFD 不敏感，RepoPath 统一为 NFC（与 git 在 macOS 上的 `core.precomposeunicode` 一致），这是同一个文件而非改名 |
| tgrep-core/src/walker.rs::metadata_walk_reports_precise_same_second_versions | 元数据遍历给出的文件「版本」含亚秒时间：同一秒内两次写入（size 与粗 mtime 相同）版本也不同 | apfs | 改写 | `Attrs` 须带纳秒级 mtime/ctime 与 `gen_count`，同一秒内的两次不同写入必须得到不同版本；对应 apfs.md §7 `gen_count_changes_on_write_not_on_touch` |
| tgrep-core/src/walker.rs::metadata_walk_detects_same_size_write_with_restored_mtime | 同尺寸改写且 mtime 被还原时，文件版本仍不同（Unix） | apfs | 改写 | 判变不能只看 size 与 mtime（构建工具、`touch -r` 会还原 mtime）：以 `gen_count`（写入才变、touch 不变）为依据，对应 `gen_count_changes_on_write_not_on_touch`；overlay 的全量对账再以哈希兜底（见 `hints_are_bounded_and_full_refresh_repairs_unreported_same_stat_edits` 行） |
| tgrep-core/src/walker.rs::walk_dir_no_excludes_returns_all_files | 没有排除项时遍历返回全部文件（相对根的路径） | apfs | 改写 | `apfs::walk(root, filter)` 在 filter 全放行时列出全部常规文件，路径相对根 |
| tgrep-core/src/walker.rs::walk_dir_retains_binary_extensions_for_file_listing | 二进制扩展名的文件不进内容候选，但仍在文件名清单里 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：内容与文件名成员的分流属搜索视图规则；`apfs::walk` 与 overlay 不按扩展名过滤（二进制文件同样是改动） |
| tgrep-core/src/walker.rs::size_limit_requires_successful_metadata | 大小上限判定：元数据读取成功且大小超限才算超限，元数据失败原样传出，无上限时不读元数据 | — | 不保留 | 实现细节：`exceeds_size_limit` 保护逐文件 stat；`getattrlistbulk` 随批返回 size，不需要逐文件 stat |
| tgrep-core/src/walker.rs::walk_dir_exclude_single_dir | 排除目录名 `vendor` 后其下文件消失，其余保留 | apfs | 改写 | 对应 apfs.md §7 `walk_excludes_configured_dirs`：按配置的目录名排除，其余不受影响 |
| tgrep-core/src/walker.rs::walk_dir_exclude_multiple_dirs | 同时排除多个目录名 | apfs | 改写 | 同上 |
| tgrep-core/src/walker.rs::walk_dir_exclude_nonexistent_dir_is_noop | 排除一个不存在的目录名等于不排除 | apfs | 改写 | 同上 |
| tgrep-core/src/walker.rs::walk_dir_exclude_skips_nested_files | 被排除目录之下嵌套的子目录与文件一并跳过（整棵子树剪枝） | apfs | 改写 | 同上 |
| tgrep-core/src/walker.rs::walk_dir_can_collect_gitignore_files_without_indexing_them | 遍历可单独收集 `.gitignore` 的路径而不把它们当内容索引（隐藏文件默认不列入；`p4ignore.ini` 命中它们也不影响收集） | gitio | 改写 | 规则来源文件的收集独立于成员清单与可见性：被隐藏或被其他规则命中的 `.gitignore` 仍贡献规则；`p4ignore.ini` 的部分不保留 |
| tgrep-core/src/walker.rs::walk_dir_collects_and_indexes_gitignore_when_hidden_included | `include_hidden` 时 `.gitignore` 既被收集也作为文件列入 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：`include_hidden` 是搜索视图的成员选项 |
| tgrep-core/src/walker.rs::build_gitignore_matcher_from_discovered_files | 由遍历发现的 `.gitignore` 构建匹配器：根的 `*.log` 与 `src/.gitignore` 的 `*.tmp` 各在其作用域内生效 | gitio | 改写 | 嵌套规则各在自己目录内生效，同 `nested_gitignore_rules_stay_scoped_to_their_directory`；API 换成 `Git::is_ignored` |
| tgrep-core/src/walker.rs::walk_dir_collects_dot_ignore_files_separately | `.ignore` 文件单独收集，不进 `gitignore_files`，以便按优先级叠加 | — | 不保留 | `.ignore` 是 ripgrep/tgrep 的约定，非 git 规则（理由同 `builds_matcher_from_dot_ignore_without_a_git_repo`） |
| tgrep-core/src/walker.rs::dot_ignore_applies_without_a_git_repo | 没有 `.git` 时 `.gitignore` 不生效而 `.ignore` 生效 | — | 不保留 | `.ignore` 非 git 规则；git 闸门在会话工作区恒开（`.git` gitfile，设计 §6.8） |
| tgrep-core/src/walker.rs::gitignore_applies_in_subdir_of_git_repo | 以仓库子目录为服务根时，祖先的 `.gitignore`、`.ignore`、`info/exclude` 作为父规则生效，并有明确的优先级 | — | 不保留 | git-strata 的根恒为仓库根（RepoPath 相对仓库根），没有「子目录作服务根」的场景；`.ignore` 非 git；嵌套优先级另见 `deeper_gitignore_negation_overrides_a_shallower_ignore` 行 |
| tgrep-core/src/walker.rs::parent_ignore_crosses_repository_boundary_but_gitignore_does_not | 仓库之外的上层 `.gitignore` 不作用于仓库内文件（上层 `.ignore` 则会跨界）；放开 `no_require_git` 后才生效 | gitio | 改写 | 只取 `.gitignore` 这半边：会话根 `<root>/sessions/<ticket>/` 不是仓库，其上的 `.gitignore` 不得作用于各 repo 工作区；`.ignore` 的跨界行为与 `no_require_git` 不保留 |
| tgrep-core/src/walker.rs::walk_file_metadata_includes_nonignored_ignore_sources | 元数据遍历把规则来源文件（`.gitignore`、`.ignore`）列为成员并另行收集，`.ignore` 默认不可见 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：规则来源文件既是视图成员又被单独收集，用于重建匹配器 |
| tgrep-core/src/walker.rs::walk_file_metadata_excludes_dirs | 元数据遍历同样按目录名排除 | apfs | 改写 | 同 `walk_dir_exclude_single_dir`：git-strata 只有一个 `walk`，全量对账与增量共用 |
| tgrep-core/src/walker.rs::walks_respect_root_p4ignore | 根目录的 `p4ignore.ini` 对遍历与元数据遍历都生效 | — | 不保留 | Perforce 约定，非 git（常量 `P4IGNORE_FILENAME` 为 `p4ignore.ini`） |
| tgrep-core/src/walker.rs::no_ignore_disables_p4ignore_for_walks_and_metadata | `no_ignore` 同时关闭 `p4ignore.ini` | — | 不保留 | 同上 |
| tgrep-core/src/walker.rs::no_require_git_applies_gitignore_outside_a_git_repo | `no_require_git` 放开「`.gitignore` 仅在 git 仓库内生效」的闸门，供 Perforce 或源码包等非 git 树使用 | — | 不保留 | 会话工作区恒是 git 仓库，没有非 git 树，该选项不存在 |
| tgrep-core/src/walker.rs::metadata_walk_honours_no_require_git | 元数据遍历与索引遍历同样遵守 `no_require_git` | — | 不保留 | 同上 |
| tgrep-core/src/walker.rs::metadata_walk_honours_a_raised_max_file_size | 元数据遍历的大小上限与索引遍历一致：低于文件大小的上限会隐藏它，调高后必须重新可见，否则陈旧检查会把它当成已删除 | overlay | 改写 | tgrep 以大小上限决定成员资格；git-strata 的 overlay 成员不受上限影响，上限只决定是否进搜索索引（设计 §7：超阈值文件只记哈希与克隆）。绝不能因超限被判为已删除（墓碑），否则发布会删文件 |
| tgrep-core/src/walker.rs::walks_default_to_a_64_mib_size_cap | 索引遍历与元数据遍历的默认大小上限同为 64 MiB，63 MiB 的文件收入、65 MiB 的文件舍弃 | search | 改写 | 搜索索引阈值默认 64 MiB（设计 §7），边界两侧的断言保留；overlay 与 search 读同一个配置值，不各写各的；超限文件仍在 overlay 里 |
| tgrep-core/src/walker.rs::meta_walk_options_default_matches_the_indexing_walk | 两个选项结构的默认上限必须相等（`derive(Default)` 会得到「无上限」） | — | 不保留 | 实现细节：tgrep 的两个手写 `Default` 的选项结构；git-strata 只有一个阈值配置（见 `walks_default_to_a_64_mib_size_cap` 行） |
| tgrep-core/src/walker.rs::ignorecase_hides_what_git_hides_and_keeps_what_git_tracks | `core.ignorecase` 为真时规则按 git 的方式不区分大小写：`QLogs` 隐藏 `qlogs/`，`*.txt` 隐藏未跟踪的 `Gone.TXT`，但已跟踪的 `Kept.TXT` 不被隐藏 | gitio | 改写 | 对应 gitio.md §7 `index_parse_tracked_never_ignored`：已跟踪文件永不被 ignore（设计 §3 大小写行）；`is_ignored` 在 ignorecase 仓库不区分大小写匹配，并先查 `IndexInfo.tracked` |
| tgrep-core/src/walker.rs::without_ignorecase_the_walk_is_unchanged | 区分大小写的仓库行为不变：不额外隐藏任何大小写变体 | gitio | 改写 | `ignorecase` 闸门的另一半：`is_ignored` 在区分大小写的仓库只做精确匹配 |
| tgrep-core/src/walker.rs::a_tracked_file_keeps_its_ignored_directory_walkable | 被规则命中的目录若含已跟踪文件则不能剪枝（git 仍报告其中的已跟踪文件），同目录里未跟踪的被忽略文件仍排除 | overlay | 改写 | overlay 的 walk filter 不得剪掉含已跟踪文件的被忽略目录，否则其中已跟踪文件的改动会漏记；丢失未发布改动是设计的反目标 |
| tgrep-core/src/walker.rs::the_point_query_matcher_hides_exactly_what_the_walk_hides | 监听用的点查匹配器与全树遍历隐藏的集合必须一致（含已跟踪豁免），否则监听订阅并索引遍历排除的树，下次陈旧检查再把它们逐出 | overlay | 改写 | overlay 的增量路径（脏路径经 `gitio::is_ignored` 点查）与全量对账（`apfs::walk` 加 filter）的判定必须一致；对应 overlay.md §7 `reconcile_after_kernel_dropped_equals_cold_scan`（Review Focus 2） |
| tgrep-core/src/walker.rs::the_point_query_matcher_follows_the_case_sensitivity_gate | 点查匹配器同样遵守区分大小写闸门：区分大小写的仓库什么也不隐藏 | gitio | 改写 | 同 `without_ignorecase_the_walk_is_unchanged` |
| tgrep-core/src/walker.rs::public_matcher_lazily_freezes_tracked_exemptions_on_first_match | 匹配器构造时不读 index，首次匹配时冻结已跟踪集合，之后不再随 index 变化 | — | 不保留 | 实现细节：tgrep `IgnoreMatcher` 的惰性冻结；`gitio::read_index` 返回值类型快照，天然不可变 |
| tgrep-core/src/walker.rs::frozen_tracked_exemption_is_immutable_and_detects_changes | 对账专用的冻结快照在一轮之内判定不变，指纹轮询可检出 index 成员变化而不改变判定 | — | 不保留 | 实现细节：tgrep 的冻结快照与成员指纹；git-strata 每轮读一次 `read_index` 值快照，变化检测见 `case_insensitive_ignore_snapshot_tracks_index_membership_coherently` 行 |
| tgrep-core/src/walker.rs::tracked_fingerprint_includes_whitelisted_files_under_ignored_directories | 已跟踪成员指纹包含「被忽略目录下由白名单放行的已跟踪文件」，该文件失去跟踪时指纹随之变化 | — | 不保留 | 同上 |
| tgrep-core/src/walker.rs::no_ignore_turns_the_whole_thing_off | `no_ignore` 关闭全部忽略规则 | — | 不保留 | tgrep 的 `--no-ignore` 搜索选项；`strata grep` v1 不暴露，overlay 与 `is_ignored` 也没有总开关 |
| tgrep-core/src/walker.rs::no_ignore_vcs_turns_the_whole_thing_off_too | `no_ignore_vcs` 同样关闭全部（含大小写不敏感那一遍） | — | 不保留 | 同上（`--no-ignore-vcs`） |
| tgrep-core/src/walker.rs::no_ignore_exclude_leaves_the_repository_exclude_file_unread | `no_ignore_exclude` 时不读 `info/exclude` | — | 不保留 | 同上（`--no-ignore-exclude`）；`info/exclude` 的读取本身见 `the_repository_exclude_is_found_through_a_worktree_pointer` 行 |
| tgrep-core/src/walker.rs::no_ignore_parent_leaves_a_subdirectory_walk_alone | `no_ignore_parent` 关闭父级规则 | — | 不保留 | 同上（`--no-ignore-parent`） |
| tgrep-core/src/walker.rs::an_unreadable_index_excludes_nothing | index 不可读时无法区分已跟踪与未跟踪，遍历宁可不排除也不猜 | gitio | 改写 | 语义保留「不确定就不猜」，行为改为返回显式错误（`read_index` 失败即停，调用方保留现场并在 status 报出，设计不变量 6），而不是静默放行 |
| tgrep-core/src/walker.rs::both_walks_agree_on_what_the_tree_contains | 索引遍历与元数据遍历对「树里有什么」必须一致，否则每次构建索引的文件都会被下次陈旧检查逐出 | overlay | 改写 | 同 `the_point_query_matcher_hides_exactly_what_the_walk_hides`：overlay 的全量遍历与增量路径成员一致（对应 `reconcile_after_kernel_dropped_equals_cold_scan`） |
| tgrep-core/src/walker.rs::hidden_visibility_matches_walks_with_ignore_whitelists_and_scoped_roots | 隐藏文件可见性与遍历一致：点文件默认隐藏，`.gitignore` 的 `!.public.txt` 白名单放行，`.github/.ignore` 对其子树生效；限定子目录范围时结论与以该目录为根的遍历相同 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：`files`/`grep` 的隐藏与范围规则（含 `.ignore`）在库内，search 经库获得 |
| tgrep-core/src/worktrees/tests.rs::managed_batches_reuse_exact_contents_copies_and_overlay_postings | 受管对账：与基线内容完全相同的文件复用基线 postings，内容相同的拷贝文件复制基线 postings，改动文件进 overlay；二次对账复用 overlay；每步与全扫结果一致，并带批次、耗时、内存计量 | — | 不保留 | 实现细节：`refresh_controlled` 的 managed v2 工作许可与计量（`peak_private_bytes`、`control_checks`）；复用语义与全扫一致性由 `identical_tracked_files_share_reader_without_extraction` 与 `serving_verification_keeps_unchanged_views_ready_and_repairs_missed_changes` 行承接 |
| tgrep-core/src/worktrees/tests.rs::provisional_base_serves_first_attach_before_full_content_verification | 首次 attach 时先用未验证的「临时基线」提供查询（`stale_reads`），完整内容验证完成后才就绪并反映工作区 | — | 不保留 | tgrep 的低延迟服务模式（`stale_reads` 加 `bootstrap_base_controlled`；库默认关闭，由 tgrep-cli 的受管服务开启）；git-strata 的 search 只在已对账的视图上作答，否则扫描（`SHARED_WORKTREE_INDEXES.md`：never silently answer from the base alone） |
| tgrep-core/src/worktrees/tests.rs::managed_crlf_overlay_fits_its_existing_budget_through_checkpoint_and_refresh | CRLF 检出（`eol=crlf` 加 autocrlf）下 8 个文件各自进 overlay，对账、checkpoint、再对账都不超出私有内存预算 | — | 不保留 | 实现细节：managed v2 的内存预算计量；CRLF 语义本身见 `crlf_and_smudge_checkouts_are_verified_even_when_git_reports_clean` 行 |
| tgrep-core/src/worktrees/tests.rs::managed_changed_files_fit_the_same_private_budget_at_any_worker_width | 受管对账在任意 worker 宽度下，改动的大文件仍不超出同一私有预算（含故障注入的暂停与重试） | — | 不保留 | 实现细节：managed v2 的预算与故障注入 |
| tgrep-core/src/worktrees/tests.rs::substantive_verification_errors_override_secondary_pressure_and_cancellation | 多个失败并存时，实质性的验证错误（IO、版本过期、超时）覆盖次要的内存压力与取消 | — | 不保留 | 实现细节：managed v2 的错误类别优先级 |
| tgrep-core/src/worktrees/tests.rs::managed_and_unmanaged_parent_pins_fit_a_low_descriptor_limit | 在子进程里把 `RLIMIT_NOFILE` 降到 256，对账 512 个互不相同的父目录时，传统路径与受管路径都不耗尽文件描述符 | apfs | 改写 | macOS 进程的默认软限常为 256：`apfs::walk` 与对账读取遍历 ≥512 个目录时不得 EMFILE（同时持有的目录句柄数有界，不随目录总数增长） |
| tgrep-core/src/worktrees/tests.rs::low_descriptor_limit_child | 上一行的子进程入口（`#[ignore]`，只由父测试驱动）：把本进程描述符软限压到 256 后做同样的 512 目录对账 | apfs | 改写 | 同上；我们的版本也须在子进程里降限，不污染同一进程里的其他测试 |
| tgrep-core/src/worktrees/tests.rs::managed_batches_are_not_split_by_binary_extension_files | 受管对账里大小未知的二进制扩展名文件不再各占一整批（64 对 txt 与 png 共 128 个文件只需 1 批） | — | 不保留 | 实现细节：managed v2 的批次计量 |
| tgrep-core/src/worktrees/tests.rs::managed_worker_failure_drains_buffers_and_parent_handles_before_returning | 工作线程失败时先释放缓冲与父目录句柄再返回；视图保持未就绪并要求全量，预留归零 | — | 不保留 | 实现细节：managed v2 的预留记账与故障注入；「失败即关闸并强制全量重试」的语义见 `read_and_discovery_errors_close_the_gate_and_force_full_retry` 行 |
| tgrep-core/src/worktrees/tests.rs::managed_cancellation_and_policy_changes_drain_paused_directory_batches | 批次暂停期间取消或策略版本变化，目录批被排空，错误分别为 Cancelled 与 StaleVersion，视图未就绪 | — | 不保留 | 实现细节：managed v2 的取消与策略版本 |
| tgrep-core/src/worktrees/tests.rs::managed_verified_read_classifies_growth_at_blob_limit_as_unstable | 读取期间文件长到超过 blob 上限时判为不稳定文件 | — | 不保留 | 实现细节：managed 的 blob 读上限（`permit.read_limits`）；「读取期间被改动即不可信」的通用语义见 `managed_verified_read_rejects_restored_mtime_mutation_and_replacement` 行 |
| tgrep-core/src/worktrees/tests.rs::managed_verified_read_preserves_blob_limit_for_stable_oversized_file | 稳定的超限文件报 `checkout-blob-read-limit` 资源压力原因，而不是被截断读取 | — | 不保留 | 实现细节：managed 的资源压力原因码；git-strata 对大文件的处理是「只记哈希与克隆，不进索引」（设计 §7） |
| tgrep-core/src/worktrees/tests.rs::managed_verified_read_rejects_restored_mtime_mutation_and_replacement | 读取过程中文件被改写（mtime 被还原）或被替换（改名后重建）时，该次读取判为不稳定、不被信任 | overlay | 改写 | checkpoint 哈希一个文件前后须核对 `Attrs`（`gen_count`、inode）：读取期间变化即不信任本次哈希，重读或留脏（设计 §6.2 先判变再哈希；PostToolUse 之后编辑仍可能在途） |
| tgrep-core/src/worktrees/tests.rs::linked_gitfile_matches_hidden_scan_without_exposing_metadata_directories | 链接工作树里 `.git` 指针文件按普通隐藏文件列出并可被搜索（`gitdir:`），而元数据目录（公共目录、git dir）整体排除；视图与普通遍历的成员与匹配逐项一致；`.ignore` 可隐藏它 | search | 改写 | 语义保留（元数据目录不暴露，视图与遍历一致），但 tgrep 会把 `.git` 指针文件当普通隐藏文件列出；git-strata 的工作区 `.git` 同为 gitfile，search 须把 `.git` 整个排除出 `files`/`grep` 结果（overlay 同样排除，设计 §6.2） |
| tgrep-core/src/worktrees/tests.rs::identical_tracked_files_share_reader_without_extraction | 两个视图（主 checkout 与链接工作树）钉同一 generation：共享同一个 `IndexReader` 与 `Generation`（`Arc::ptr_eq`），与基线相同的文件零提取、不进 overlay，首次对账前视图未就绪，结果与全扫一致 | search | 改写 | 对应 search.md §7 `base_index_shared_across_sessions_same_tree`：同 tree 的会话共享基线索引，干净文件不进私有 overlay；tgrep 里链接工作树的 `.git` 指针文件多出 1 次私有提取，search 排除 `.git` 后应为 0 |
| tgrep-core/src/worktrees/tests.rs::divergent_commits_staging_renames_deletes_and_whole_file_overlays_are_isolated | 工作树 A 相对钉住基线的整文件差异互相隔离：会话内提交的分歧不使差异消失，暂存与未暂存都按当前内容，改名等于旧路径墓碑加新路径条目，删除等于墓碑，未跟踪新增进 overlay，跨编辑边界的匹配成立；B 仍见自己的版本；提交后覆盖保留，改回基线内容后覆盖消失 | search | 改写 | 对应 search.md §7 `overlay_entry_shadows_base_path`（A 改过的路径即使新内容不再匹配也遮住基线）与 `tombstone_hides_base_in_this_session_only`（A 的墓碑只在 A 内隐藏基线，B 不受影响）；尾段「改回基线内容则覆盖消失」对应 overlay.md §7 `restored_to_base_removes_override`，「删除即墓碑」对应 `deleted_base_file_is_tombstone` |
| tgrep-core/src/worktrees/tests.rs::hints_are_bounded_and_full_refresh_repairs_unreported_same_stat_edits | 提示队列有界（容量 1，溢出即要求全量）；未被通知、同 stat（mtime 被还原，git 标 assume-unchanged，`git status` 干净）的改写，增量刷新看不见而 `reconcile_full` 一定修复；越界提示强制全量；每次无提示刷新得到新的发布世代 | overlay | 改写 | 设计 §6.2「正确性不依赖监听」：脏路径集溢出或事件丢失（`WatchEvent::Dropped`）时退化为全量；全量对账不信 mtime、size、`git status`，逐文件按哈希核对，结果等于冷扫（对应 `reconcile_after_kernel_dropped_equals_cold_scan`） |
| tgrep-core/src/worktrees/tests.rs::membership_visibility_binary_size_and_storage_rules_follow_the_requesting_root | `files`/`grep` 的成员资格按请求者自己的根判定：该工作树自己的 `.gitignore`/`.ignore`、隐藏文件与隐藏目录、二进制扩展名与内容、大小上限、存储目录（`.tgrep`、检查点目录、自定义排除）；改名或删规则后重新对账即变化；另一个根看到不同结果 | search | 改写 | search 的成员资格按会话自己的工作区判定，不继承基线或他人。⚠ tgrep 对「已跟踪但被工作区 `.gitignore` 命中」的文件也排除（本例 `ignored.txt`），而 overlay 与 gitio 是「已跟踪永不 ignore」，在区分大小写的仓库里两者不一致，需在 search.md 里定夺 |
| tgrep-core/src/worktrees/tests.rs::crlf_and_smudge_checkouts_are_verified_even_when_git_reports_clean | 开启 autocrlf 与 smudge/clean 过滤器后，`git status` 干净但检出字节与 git blob 不同：视图按解码后的实际字节核对，4 个文件加 `.git` 指针全部私有提取，匹配与全扫一致，再对账零提取 | gitio | 改写 | git blob 不等于检出字节（autocrlf、eol、smudge）：`gitio::export_tree` 导出基线须按仓库配置转换，BaseManifest 的 BlobId 取导出后的字节，overlay 比字节而不看 `git status`；验收口径：导出后工作区 `git status --porcelain` 为空。search 侧的解码核对由库承担 |
| tgrep-core/src/worktrees/tests.rs::checkout_decoding_can_reuse_base_despite_different_raw_bytes | 文件被改存为带 BOM 的 UTF-16LE 但解码后文本相同，则复用基线 postings、零提取 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：按解码后身份复用 postings 是库内索引优化；注意 overlay 以字节为准，重新编码算改动 |
| tgrep-core/src/worktrees/tests.rs::sparse_missing_and_skip_worktree_paths_never_leak_base_terms | sparse-checkout 未物化的路径记墓碑（基线词条不泄漏），`skip-worktree` 文件以目录里的当前内容为准，关闭 sparse 后再对账即可见 | overlay | 改写 | overlay 只比较目录与基线清单：基线有而目录无即墓碑（对应 `deleted_base_file_is_tombstone`），不信 git 的 sparse 与 skip-worktree 标志；⚠ 会话内启用 sparse-checkout 会把未物化路径记成墓碑，发布前须人工核对 |
| tgrep-core/src/worktrees/tests.rs::epoch_change_after_preparation_prevents_stale_ready_publication | 对账准备完成后、发布前又来了变更：不得发布过期的就绪状态（返回 ChangedDuringReconcile），视图保持未就绪并要求全量，重试后反映新内容 | overlay | 改写 | checkpoint 与对账进行中到达的新脏路径不能丢：每轮取脏集快照，处理期间新增的留给下一轮；同一会话的 checkpoint 串行（设计 §8） |
| tgrep-core/src/worktrees/tests.rs::stale_reads_serve_published_state_and_keep_newer_hints_after_churn | `stale_reads` 模式下视图未就绪时仍对外提供上次发布的状态，并保留对账期间到来的更新提示 | — | 不保留 | tgrep 的 `stale_reads`（未就绪仍作答；库默认关闭，由 tgrep-cli 的受管服务开启）；git-strata 的 search v1 不开启，视图未就绪时先 refresh 或退化为扫描；若 grep 时延目标（< 300 ms，设计 §8）需要再评估 |
| tgrep-core/src/worktrees/tests.rs::stale_read_open_failures_queue_hints_instead_of_failing_queries | `stale_reads` 下候选文件打开失败时转为排队提示而不使查询失败 | — | 不保留 | 同上 |
| tgrep-core/src/worktrees/tests.rs::stale_read_pass_failures_keep_serving_and_queue_full_repair | `stale_reads` 下一轮对账失败仍继续提供旧状态并排队全量修复 | — | 不保留 | 同上 |
| tgrep-core/src/worktrees/tests.rs::unchanged_file_requires_equal_trusted_evidence_and_hidden_status | 「文件未变」的快速判定要求属性与上次已验证的证据完全相等（含 Windows 隐藏属性）；未知路径、越界路径、目录元数据一律判为变化 | — | 不保留 | 实现细节：`unchanged_file` 的可信证据比对只被 tgrep-cli 受管服务（`serve/shared/managed.rs`）调用；git-strata 的判变依据是 `Attrs`（`gen_count` 等）与上次 checkpoint 记录的比较（见 `gen_count_changes_on_write_not_on_touch`） |
| tgrep-core/src/worktrees/tests.rs::read_and_discovery_errors_close_the_gate_and_force_full_retry | 对账中读取失败（文件被删）或忽略规则出错（畸形的 `[z-a]`，IncompleteWalk）时视图变为未就绪，修复后重试才就绪并反映新内容 | search | 改写 | search 的 `refresh` 失败时视图不得用基线或旧状态作答（`SHARED_WORKTREE_INDEXES.md`：缺 checkpoint 或基线时用已对账的视图或扫描，never silently answer from the base alone）；grep 在未就绪视图上须先重试对账或退化为扫描，并报出原因 |
| tgrep-core/src/worktrees/tests.rs::actual_bytes_not_discovery_size_decide_size_and_binary_classification | 大小与二进制分类以实际读到的字节为准，而不是遍历时的元数据（遍历后文件变小、变大或含 NUL） | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：库内索引资格判定 |
| tgrep-core/src/worktrees/tests.rs::snapshot_sibling_batches_reject_foreign_roots_and_record_open_failures | 快照里的候选批只能在其所属视图使用（别的视图的批被拒），批内打开失败使视图失效并要求全量 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：库内 `WorktreeSnapshot` 的批 API；search 经 `with_snapshot` 使用时的失败路径由库保证 |
| tgrep-core/src/worktrees/tests.rs::snapshot_candidate_opens_reject_directory_link_swaps | 查询期间候选文件的父目录被换成指向根外的链接：打开失败、视图失效并要求全量，根外内容不会出现在结果里 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：search 的最终匹配必须经 `snapshot.open_file`（不得 `root.join(path)` 直读），由此获得该防护 |
| tgrep-core/src/worktrees/tests.rs::snapshot_open_failures_require_full_repair_even_if_the_callback_ignores_them | 即便回调吞掉了打开错误，快照仍使视图失效并要求全量修复，错误信息只含第一个失败路径 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：库内 `with_snapshot` 的错误闩锁 |
| tgrep-core/src/worktrees/tests.rs::snapshot_fifo_swaps_invalidate_readiness_without_blocking | 查询期间文件被换成 FIFO：打开不阻塞、视图失效并要求全量（子进程加截止保护） | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：库内查询路径；我们自己的读取路径见 `rooted/tests.rs` 的 apfs 行 |
| tgrep-core/src/worktrees/tests.rs::snapshots_reject_root_replacement_before_and_during_empty_queries | 视图的根目录在查询前或查询中被整体换走（改名后重建同名目录）：即使是空查询也报错，视图失效并要求全量；换回原目录后重新对账才恢复 | search | 改写 | `session::move_workspace` 与发布后 rebase 会用 `rename` 整体交换工作区目录（设计 §6.4）；此后旧视图必须拒绝服务并要求重新 attach，不得对新目录里的同名文件作答 |
| tgrep-core/src/worktrees/tests.rs::directory_link_swap_after_discovery_keeps_the_view_not_ready | 对账发现之后中间目录被换成指向根外的链接：对账失败，视图未就绪，修复后不含根外内容 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：库内对账期间的链接替换防护；同一语义在我们自己的读取路径上见 `rooted/tests.rs` 的 apfs 行 |
| tgrep-core/src/worktrees/tests.rs::root_replacement_before_publication_keeps_the_view_not_ready | 对账发布前根目录被换走：不发布，视图保持未就绪 | search | 改写 | 同 `snapshots_reject_root_replacement_before_and_during_empty_queries`（refresh 期间发生 rename 交换）：对账与 `move_workspace` 须互斥，或事后重新 attach |
| tgrep-core/src/worktrees/tests.rs::observed_parent_replacement_after_batch_drain_blocks_publication | 批次排空后被观察的父目录被换掉：阻止发布，修复后全量重读且无内容复用（受管与非受管各跑一遍） | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：库内批次祖先复核 |
| tgrep-core/src/worktrees/tests.rs::fifo_swap_after_discovery_is_bounded_and_keeps_the_view_not_ready | 对账发现之后文件被换成 FIFO：有界返回错误，视图未就绪 | — | 依赖承担 | 同上 |
| tgrep-core/src/worktrees/tests.rs::restored_checkpoints_revalidate_contents_membership_and_exact_generation | 视图把自己的 delta 存成 `overlay.json`（仅 delta，绑定 generation 键与根），恢复后视图仍未就绪、须重新对账，旧 delta 内容不被信任；generation 不符、根不符、JSON 损坏均拒绝 | — | 不保留 | 实现细节：tgrep 搜索视图私有的 checkpoint 文件；git-strata 的 checkpoint 在 store（SQLite），search 在 attach 时由 overlay 重建视图，不用 `save_checkpoint`/`restore`；「重启后视图须重新对账才可服务」见 `identical_tracked_files_share_reader_without_extraction` 行的未就绪断言 |
| tgrep-core/src/worktrees/tests.rs::checkpoint_configuration_rejects_regular_files | checkpoint 目录配置为普通文件时，构造与恢复都被拒且该文件原样保留 | — | 不保留 | 实现细节：tgrep 视图的 `checkpoint_directory` 配置校验 |
| tgrep-core/src/worktrees/tests.rs::checkpoint_save_rejects_a_directory_replaced_by_a_file | 保存时 checkpoint 目录已被普通文件替换则拒绝，且不覆盖该文件 | — | 不保留 | 同上 |
| tgrep-core/src/worktrees/tests.rs::registration_rejects_other_repositories_subdirectories_and_immutable_checkpoint_paths | 视图只接受该仓库的工作树根（别的仓库、子目录被拒），checkpoint 目录不能是工作树根、generation 目录或 `.git`；拒绝发生时基线索引字节不变 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：search 的 attach 由会话清单给出受信的根；库内校验与「基线索引永不被改写」（设计不变量 5）由库保证 |
| tgrep-core/src/worktrees/tests.rs::subtree_hints_and_case_aliases_reverify_unchanged_metadata | 对目录的提示（含大小写别名 `DIR`）使其下所有文件逐个核对内容，即使元数据未变（同 stat 改写也被发现）；非 ASCII 提示（如 é）保守地要求全量 | overlay | 改写 | APFS 大小写不敏感且对 NFC/NFD 不敏感，FSEvents 报告的路径拼写可能与记录的不同（`DIR` 对 `dir`、NFD 对 NFC）：overlay 把事件路径按 `apfs::case_fold_eq` 与 NFC 归并到已知路径并重新核对内容，无法确认别名时整树重扫（这也是 `rename_case_only_recorded_as_rename` 的前置条件） |
| tgrep-core/src/worktrees/tests.rs::subtree_hint_spellings_reverify_restored_mtime_edits | 目录提示的各种拼写（`dir/`、`dir//`、`dir/.`、`dir//./`、`dir/./nested//` 等）都使整棵子树逐文件核对，mtime 被还原的同尺寸改写也被发现；未改动的兄弟文件同样被核对 | overlay | 改写 | 监听给出的目录事件是「子树脏」信号：任何拼写归一后都触发整棵子树核对，防止丢失子树提示；同 stat 改写靠哈希兜底 |
| tgrep-core/src/worktrees/tests.rs::accepted_hint_spellings_have_one_canonical_queue_entry | 同一目录的多种拼写在队列里只产生一条规范条目；空串、`.`、`./dir`、`../dir`、`dir/../file` 无效并强制全量 | overlay | 改写 | 脏路径集按规范化的 RepoPath 去重；无法规范化的路径不静默忽略，改为强制全量对账 |
| tgrep-core/src/worktrees/tests.rs::hint_lookup_work_depends_on_path_depth_not_queue_size | 提示命中检查的开销取决于路径深度而非队列规模（2001 条无关提示下仍只查 5 次），且 `dir` 不命中 `directory/` | — | 不保留 | 实现细节：`hint_lookups` 计数器与有序集合的前缀探测；checkpoint 时延目标（改 50 个文件 < 200 ms，设计 §8）由验收基准承担；前缀止于分隔符另见 `finds_tracked_files_under_a_directory` 行 |
| tgrep-core/src/worktrees/tests.rs::non_utf8_checkout_paths_cannot_alias_literal_replacement_paths | 非 UTF-8 文件名不得与其 U+FFFD 替换形态的路径互为别名：遍历报告不完整，不读取被忽略的别名文件，移除后才就绪 | — | 不保留 | APFS 拒绝创建非 UTF-8 文件名（EILSEQ，errno 92），测试在 macOS 上于创建夹具处提前 return；该场景在 macOS 不会出现 |
| tgrep-core/src/worktrees/tests.rs::literal_backslash_checkout_paths_cannot_alias_directory_paths | 文件名含字面反斜杠（`ignored\alias.txt`）不得与目录路径 `ignored/alias.txt` 互为别名：视图报告遍历不完整并保持未就绪，移除该文件后恢复 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：macOS 上字面 `\` 是合法文件名字符，该场景可出现；库对它 fail-closed（视图无法就绪），search.md 的失败路径须写明「退化为扫描并报出该文件名」；overlay 与 RepoPath 把 `\` 当普通字符，见 `index_keys_never_rename_a_native_name` 行 |
| tgrep-core/src/worktrees/tests.rs::non_utf8_directory_paths_are_rejected_before_visibility_or_descent | 非 UTF-8 目录名在记录可见性或下降之前即被拒绝，遍历计入 1 个错误，视图不就绪 | — | 不保留 | 同 `non_utf8_checkout_paths_cannot_alias_literal_replacement_paths`：APFS 拒绝创建非 UTF-8 目录名，测试在 macOS 上提前 return |
| tgrep-core/src/worktrees/tests.rs::no_ignore_still_excludes_private_storage_and_larger_view_can_overlay_capped_base | `no_ignore` 仍排除 `.git/`、`.tgrep/`、自定义排除路径与 checkpoint 目录；视图的大小上限大于基线 profile 的上限时，基线跳过的大文件由 overlay 补上 | — | 依赖承担 | tgrep-core 作为库依赖，其自身测试覆盖：库内成员与存储目录排除规则 |
| tgrep-core/src/worktrees/tests.rs::windows_hidden_attributes_and_unreadable_files_invalidate_correctly | Windows 隐藏属性变化与独占打开的不可读文件使视图正确失效 | — | 不保留 | Windows 专属（`#[cfg(windows)]`）；Windows 是设计的非目标（设计 §2） |
| tgrep-core/src/worktrees/tests.rs::case_insensitive_ignore_snapshot_tracks_index_membership_coherently | `core.ignorecase` 为真时，`.gitignore` 的小写规则不隐藏已跟踪的 `TRACKED.txt`；`git rm --cached` 之后它变为未跟踪并被忽略，刷新后消失；index 被写坏后刷新报错、视图未就绪 | gitio | 改写 | 对应 gitio.md §7 `index_parse_tracked_never_ignored`：已跟踪集合以当前 index 为准，取消跟踪立即改变 ignore 判定；index 损坏返回显式错误，而不是「无已跟踪文件」 |
| tgrep-core/src/worktrees/tests.rs::symlinks_executable_modes_and_materialized_gitlinks_match_walker | 符号链接不跟随也不按内容索引：常规文件被换成符号链接后其旧词条被遮住，符号链接换成常规文件后按内容出现；可执行位不影响；已物化的 gitlink 目录里的文件可见；全部与遍历一致 | overlay | 改写 | 常规文件与符号链接之间的类型变化是一条改动（`OverlayEntry.mode` 记符号链接，旧内容被遮蔽，链接不跟随、不按内容入库）；可执行位只影响 mode；gitlink（子模块）设计未涉及 |
| tgrep-core/src/worktrees/tests.rs::serving_verification_keeps_unchanged_views_ready_and_repairs_missed_changes | 对就绪视图做服务期全量核验：无差异时保持就绪、不重新发布、查询不中断；发现漏报的同 stat 改写（assume-unchanged，`git status` 干净）、新目录里的新增、删除、`.gitignore` 变更、改回基线内容时，先关闭就绪再发布修复结果，结果与全扫一致；之后再核验为 Unchanged | overlay | 改写 | 设计 §6.2 发布前与每小时的全量对账：不信监听、mtime、`git status`，逐文件按哈希核对；无差异不产生新 checkpoint；漏报的各类改动（同 stat 改写、新增、删除、规则变化、改回基线）都被修复；对应 `reconcile_after_kernel_dropped_equals_cold_scan` 与 `restored_to_base_removes_override` |
| tgrep-core/src/worktrees/tests.rs::serving_verification_yields_to_invalidations_and_fails_closed | 核验让位于待处理的失效（有提示或未就绪时不核验）；核验期间到来的事件保留其提示；已发现差异后再来事件则需全量；核验读失败按失败关闭处理 | overlay | 改写 | 定时全量对账让位于待处理的脏路径，期间到来的事件不丢，读取失败保持脏并在下一轮全量重来（设计 §6.2 与不变量 6）；与 `epoch_change_after_preparation_prevents_stale_ready_publication` 行同一并发语义 |
| tgrep-core/src/worktrees/tests.rs::managed_serving_verification_stays_available_and_respects_budget_semantics | 受管的服务期核验：核验过程中视图保持就绪并对外服务；取消不证明任何事（保持就绪），策略版本变化则失败关闭 | — | 不保留 | 实现细节：managed v2 的预算、取消与策略版本 |
| tgrep-core/src/worktrees/tests.rs::managed_serving_verification_budget_failure_after_a_difference_fails_closed | 受管核验已发现差异后预算失败（取消）：先关闭就绪，预算失败不会恢复就绪，之后核验为不适用 | — | 不保留 | 同上 |
| tgrep-core/src/worktrees/tests.rs::content_only_profile_admits_extension_named_text_through_base_overlay_and_hints | `ContentOnly` eligibility profile：扩展名像二进制但内容是文本的文件（`.doc`、`.pdf` 等）被收入基线、overlay 与提示路径；checkpoint 绑定确切 profile | — | 不保留 | tgrep 的可选 eligibility profile；git-strata v1 固定默认 profile（按扩展名排除二进制），不暴露 ContentOnly |
| tgrep-core/tests/case_insensitive_roundtrip.rs::case_insensitive_search_roundtrip | 用 ASCII 小写补充三元组后写出索引再读回，大小写不敏感的正则查询、大小写不敏感的字面量查询与区分大小写的查询都能命中该文件 | — | 依赖承担 | search 的 grep 候选筛选经库的 `query::build_query_plan` / `build_literal_plan` 与 `IndexReader` 完成，由 tgrep-core 自身测试覆盖；本例不涉及文件名的大小写规则（`core.ignorecase` 见 `walker.rs` 与 `git_index.rs` 的行） |
| tgrep-core/tests/corrupt_index_reader.rs::reader_rejects_unsafe_stored_paths_before_queries | 索引文件表里存的路径是 `../outside.txt`、`/outside.txt` 或空串（Windows 上还有盘符形式）时，`IndexReader::open` 与 `HybridIndex::open` 都在查询之前报 `IndexCorrupted` | — | 依赖承担 | search 经库的 `IndexReader::open` / `HybridIndex::open` 打开基线索引，打开时拒绝不安全的已存路径是库的契约，由 tgrep-core 自身测试覆盖；git-strata 自己的路径入口是 `RepoPath` 的构造（types.md §3） |
| tgrep-core/tests/corrupt_index_reader.rs::builder_and_reader_round_trip_unix_colon_filenames | Unix 上含冒号的文件名（`a:b`、`C:relative.txt`、`C:/nested.txt`、`dir/name:stream`）是普通名字：建索引、读回、经 `RootedDir` 按字面打开、写出并读回额外路径文件，全部无损 | — | 依赖承担 | 建索引与读回走库路径，由 tgrep-core 自身测试覆盖；『冒号名字按字面打开，不当作盘符或数据流』同样是 apfs `open_rooted` 要满足的，见 `tgrep-core/src/rooted/tests.rs::regular_opens_and_literal_path_validation` 一行 |
| tgrep-core/tests/corrupt_index_reader.rs::adversarial_lookup_entries_are_handled_without_panic_or_hang | lookup.bin 的偏移与长度被写成对抗取值（偏移取 u64::MAX、2 的 40 次方等 12 种，长度取 0 到 u32::MAX 共 9 种，逐一组合）时，读取不 panic、不挂起（单次操作不超过 30 秒预算），解码出的条目数与预留容量都不超过 index.bin 实际装得下的数量 | — | 依赖承担 | 读取损坏或对抗性的索引不 panic、不挂起，是 `IndexReader` 的契约（与 `fuzz_reader` 同源）；search 只经库读取自己写入的基线索引，由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/corrupt_index_reader.rs::truncated_postings_still_yield_the_entries_that_fit | 条目范围只有一部分有效（从第 2 条起声称 100 条，实际只剩 4 条完整记录）时，读取仍返回真正放得下的前缀（文件号 2、3、4、5），而校验路径 `validate_lookup` 报错 | — | 依赖承担 | 同上：优雅降级而不丢数据是 `IndexReader` 的契约 |
| tgrep-core/tests/corrupt_index_reader.rs::random_index_bytes_never_panic | 固定种子的伪随机索引字节（300 轮，偏移与长度偏向越界取值）下，读取不 panic、不超预算，解码量不超过 index.bin 的容量 | — | 依赖承担 | 同上 |
| tgrep-core/tests/generations.rs::linked_worktrees_and_equal_trees_share_one_arc | 同一仓库的主工作区、链接工作树与子目录经 `Repository::discover` 得到同一个仓库身份；同一个 tree 不论来自哪个 commit、哪个链接工作树，`ensure` 都复用同一个 generation（`Arc::ptr_eq`，第二次不再提取 blob，`reused_generation` 为真），列表里只有一份，generation 记的是最先构建它的 commit；默认存储目录在 git common dir 之下而不在链接工作树里；链接工作树自己提交出的不同 tree 另建一份，可以拿前一份作前驱增量构建 | search | 改写 | 对应 search.md §7 `base_index_shared_across_sessions_same_tree`：键是（仓库身份、tree OID、索引 profile），同 tree 的会话共用一份基线索引，与各自的 commit 无关。会话工作区的 `.git` 按设计 §6.1 接到主 checkout 的 object store，仓库身份同样取 git common dir（设计 §4），从主 checkout、会话目录、子目录发现的是同一个仓库，这一点也是 gitio 的 discover 用例 |
| tgrep-core/tests/generations.rs::clone_tree_and_profile_keys_do_not_collide | 两个独立克隆里同一个 tree OID 得到不同的 generation 键与不同目录，互相不能打开对方的 generation（`Incompatible`）；索引 profile 不同（`max_blob_bytes` 上限 3 字节）也得到不同的键，超过上限的文件记为 `TooLarge`、不进索引；以 profile 不兼容的 generation 作前驱做增量被拒；文件变化后增量只提取 1 个 blob | search | 改写 | 基线索引的键必须是（仓库身份、tree OID、profile），不能只看 tree OID：两个仓库可以有相同的 tree，却不能共用一份索引；大小阈值（默认 64 MiB，设计 §7）属于 profile，阈值不同的索引互不复用；超限文件不进搜索索引，仍在 overlay 与 store 里 |
| tgrep-core/tests/generations.rs::builds_committed_blobs_not_staged_dirty_or_untracked_contents | 基线只含已提交的 blob：已暂存的新内容、工作区的脏内容、未跟踪文件都不进基线；已提交的点文件、被 `.gitignore` 命中但已跟踪的文件（`git add -f`）、扩展名像二进制但内容是文本的文件都进基线 | search | 改写 | 基线 = 钉住的 tree，暂存、脏、未跟踪的内容只属于会话 overlay（设计 §4）；已跟踪文件即使被 `.gitignore` 命中也在基线里，与『已跟踪永不 ignore』一致（gitio `index_parse_tracked_never_ignored`）。注意 tgrep 的 worktree 视图随后又会按 `.gitignore` 把这类文件藏起来，git-strata 不沿用，见 `tgrep-core/src/worktrees/tests.rs::membership_visibility_binary_size_and_storage_rules_follow_the_requesting_root` 一行 |
| tgrep-core/tests/generations.rs::incremental_reuses_masks_renames_and_copies_extracting_only_new_blobs | 新 commit 相对前驱 generation 增量构建：只读取并提取新增与改动的 2 个 blob，改名与复制的文件复用前驱的倒排与掩码，删除的文件消失；前驱仍可重新打开；与另一处独立的全量构建得到相同的 `snapshot_id` | — | 依赖承担 | search 在会话基线移动（发布后 rebase）时用库的增量构建 `ensure(…, Some(前驱))`，读取量、倒排复用与『增量结果等于全量结果』由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/generations.rs::raw_blob_semantics_require_decoded_identity_not_git_cleanliness | 基线构建不执行已配置的 checkout 过滤器（smudge 命令不存在也不报错），并按『解码后的身份』而不是 git 的干净状态判断工作区字节是否等于基线：`eol=crlf` 的文件，LF 的原始字节匹配、CRLF 的工作区字节不匹配；`filter=` 与 `working-tree-encoding` 的文件不声称匹配；带 BOM 的 UTF-16 解码后可检索；含 NUL 的标为二进制且不进索引；空文件与单字节文件有内容身份并被列出 | gitio | 改写 | 这是 `worktrees/tests.rs` 里那条 CRLF/smudge 要求的集成版：git-strata 的 `gitio::export_tree` 须像 `git checkout` 一样应用 eol、编码与 smudge，并把导出字节的哈希记入基线清单，未改动的工作区才不会被误判为脏。tgrep 靠『解码后身份』比对，git-strata 改为比对导出字节的哈希，不再需要这套比对；二进制不进搜索索引、空文件与短文件仍被列出的规则留给 search |
| tgrep-core/tests/generations.rs::same_blob_destination_mode_is_reclassified | 同一个 blob 换了模式或路径要重新归类：普通文件改成符号链接（120000）后不再提取 blob，条目变为 `Symlink` 与 `NotRegular` 且不在索引里；符号链接改回普通文件并换到新路径时，该 blob 被提取一次并可检索 | — | 依赖承担 | 构建基线索引时按（路径、模式）归类，符号链接与 gitlink 不进搜索索引，走库路径，由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/generations.rs::empty_tree_is_a_valid_complete_generation | 空提交（空 tree）也是合法完整的 generation：读 0 个 blob、0 个条目、索引 0 个文件，列表里有一份 | — | 依赖承担 | 空 tree 的基线索引构建走库路径，由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/generations.rs::simultaneous_managers_share_publication_and_reader | 6 个线程各持一个 manager 同时对同一 HEAD 调 `ensure`：只有一个发布，总共只提取 1 个 blob，其余线程拿到同一个 generation 与同一个 reader（`Arc::ptr_eq`） | search | 改写 | 与 `tgrep-core/src/generations/mod.rs::concurrent_cold_loads_recheck_before_cache_insertion` 同义，对应 search.md §7 `base_index_shared_across_sessions_same_tree`：多个会话同时 attach 同一个 tree，基线索引只建一份、全部共享 |
| tgrep-core/tests/generations.rs::process_worker | 下面两条跨进程用例（`simultaneous_processes_deduplicate_builds` 与 `killed_starter_releases_lock_and_leaves_only_undiscoverable_staging`）的子进程入口：仅当环境变量 `TGREP_GENERATION_TEST_ROOT` 存在时才工作（普通模式：等闸门文件后构建一次并写出结果；`INTERRUPT` 模式：持发布锁、留下半成品暂存目录后睡眠），单独运行时直接返回，不断言任何行为 | — | 不保留 | 测试脚手架（被父用例当子进程拉起的 worker），自身没有断言；跨进程场景见下面两行 |
| tgrep-core/tests/generations.rs::simultaneous_processes_deduplicate_builds | 4 个进程同时对同一 HEAD 建 generation：只有 1 个进程发布并提取 1 个 blob（输出 `true 1`），另 3 个复用（`false 0`），最终只有一份 generation | — | 不保留 | 跨进程靠文件锁（`publication.lock`）去重；git-strata 一台机器只有一个 daemon（设计 §5），基线索引只由 daemon 构建，不存在多进程同时构建。进程内的并发去重见 `simultaneous_managers_share_publication_and_reader` 一行 |
| tgrep-core/tests/generations.rs::killed_starter_releases_lock_and_leaves_only_undiscoverable_staging | 构建进程持发布锁、留下半成品暂存目录（`.stage-*`）时被强杀：锁没有被永久占用，暂存目录不会被当成 generation 列出，之后重新构建成功并可检索 | deps | 改写 | 对应不变量 5 与 6，以及 `tgrep-core/src/generations/mod.rs::invalid_staged_generation_is_never_published`：daemon 或安装进程在 staging 里被杀，遗留的 staging 目录不是 generation（不可发现、不被引用），锁不因持有者被杀而永久占用，重启后可重来或清理；依赖集 generation 与基线 generation 的建立同理 |
| tgrep-core/tests/generations.rs::retained_views_and_checkpoints_outlive_managers_and_new_generations | 旧 generation 被一个视图和一个 checkpoint 引用时，新 commit 产生了新 generation、manager 被丢弃之后，旧 generation 目录仍在、视图仍能读；重新打开旧 generation 后 checkpoint 可还原，并仍能检索旧内容 | store | 改写 | 对应设计 §7 的空间回收：generation 引用计数（会话清单、checkpoint、发布记录），无引用且超过保留期才删。新基线出现不等于旧基线可删，旧基线上的 checkpoint 始终能还原 |
| tgrep-core/tests/generations.rs::interrupted_and_failed_publications_are_not_discoverable | 遗留的 `.stage-*` 暂存目录不会被列出；把 `--help` 当修订名调 `ensure` 报 git 错误且不产生 generation；发布锁的位置被换成目录（读不了的锁）时 `ensure` 失败而不是无锁发布，已发布的 `index.bin` 字节不变 | deps | 改写 | 同 `killed_starter_releases_lock_and_leaves_only_undiscoverable_staging`（不变量 5 与 6：封存前不可见、失败不动已发布的 generation、拿不到锁就报错而不是绕过）；『把选项当修订名必须报错而不是执行』对应 gitio 的参数注入防护，写进 gitio.md 的 §7 |
| tgrep-core/tests/generations.rs::malformed_metadata_is_rejected_without_overwriting_generation | generation 的清单 JSON 里某个条目的 oid 被改成全 0 时，`open` 报 `InvalidMetadata`；`ensure` 失败且不覆盖这份无效的 generation（文件字节保持原样），`list` 报错；把原清单写回后可正常打开并检索 | store | 改写 | 对应不变量 6：generation 的元数据与内容对不上时，不得悄悄覆盖重建，要保留现场并报告，由人决定；修复后可恢复使用 |
| tgrep-core/tests/generations.rs::replace_refs_do_not_change_exact_committed_tree | 对某个 commit 做了 `git replace` 之后，按原 commit 构建的 generation 仍是原 commit 的 tree（命中 `original`，不命中 `replacement`），替换引用不改变被索引的确切 tree | gitio | 改写 | gitio 解析 tree OID、读取 tree 与 blob 时必须忽略 replace 引用（`GIT_NO_REPLACE_OBJECTS` 或等价做法），钉住的基线才是确切的那个 tree；写进 gitio.md 的 §7 |
| tgrep-core/tests/generations.rs::gitlinks_are_membership_only_and_missing_blobs_are_errors | 子模块条目（gitlink，模式 160000）只记成员：模式为 `Gitlink`、内容为 `NotRegular`、没有大小、不进索引；缺失的 blob 对象（手工删掉一个松散对象）使 `ensure` 失败，已发布的 generation 仍在列表里、仍可检索 | gitio | 改写 | 构建遇到缺失的对象必须整体失败，而不是跳过该文件，现有基线保持可用（不变量 6）。gitlink 这一半有缺口：types.md §4 的 mode 取值（0o100644、0o100755、0o120000）里没有 160000，gitio.md 须表态——记成无内容的成员，或明确拒绝，二者都不得静默丢掉 |
| tgrep-core/tests/generations.rs::unsupported_storage_and_discovery_return_errors | 在非 git 目录上 `Repository::discover` 报错；把存储目录指向仓库工作区、`.git` 目录或一个普通文件都被拒绝；还没有任何提交的仓库（unborn HEAD）不会被当成空 tree，而是报错 | gitio | 改写 | `strata new` 对非仓库目录与尚无提交的仓库必须明确报错，不能当成空 tree 建出基线，由 gitio 的 discover 与 rev 解析用例承担；『存储目录不能落在仓库工作区、`.git` 或文件上』是 tgrep 的存储布局检查，git-strata 的 store 位置另有规则（设计 §7），不沿用 |
| tgrep-core/tests/generations.rs::effective_storage_cannot_be_a_linked_worktree | 存储目录算出的有效路径正好是一个链接工作树时，`with_storage` 拒绝（`Unsupported`），不在其中创建 `publication.lock`，该工作树的 `git status` 保持干净；默认存储位于 git common dir 之下而不在链接工作树里 | — | 不保留 | tgrep 的存储布局（`<存储根>/<仓库身份>`）与链接工作树撞路径的检查；git-strata 的 store、基线缓存与依赖集缓存是自己的目录布局（设计 §7），会话目录是 clonefile 出来的克隆，不会和它们混用 |
| tgrep-core/tests/generations.rs::effective_storage_cannot_be_an_existing_snapshot | 有效存储路径上已经有一份索引快照（`SharedBase` 可打开）时，`with_storage` 拒绝（`Unsupported`），不创建 `publication.lock`，原快照的 id 不变 | — | 不保留 | 同上：tgrep 存储布局的碰撞检查；git-strata 的 store 里没有『一个目录既可当快照又可当存储根』的二义性 |
| tgrep-core/tests/generations.rs::unchanged_binary_or_empty_destination_does_not_scan_predecessor_postings | 增量构建时，若前驱里没有可复用的带倒排的文件（未变的只有二进制文件，或目标 tree 为空），不扫描前驱的倒排表（读取数与复用数都为 0）；改过的源文件被重新提取并可检索 | — | 依赖承担 | 增量构建的性能边界，属库内部的统计（`predecessor_posting_lists_read` 等），由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/generations.rs::posting_free_indexed_files_reuse_without_predecessor_scan | 空文件、1 到 2 字节的文件、只有 BOM 或 BOM 加 1 到 2 个字符的文件（含 UTF-8 与 UTF-16 两种 BOM；索引里没有倒排，但仍是成员）在增量构建中按身份复用，不扫描前驱的倒排；重新打开旧 generation 也能恢复『有无倒排』；`a` 这样的短模式列出全部成员；源文件被删光时索引的 trigram 数为 0 | — | 依赖承担 | 同上 |
| tgrep-core/tests/generations.rs::short_raw_bytes_with_decoded_postings_still_reuse_postings | 原始字节只有 1 字节、但解码后有倒排的文件（无效字节解码成 U+FFFD）仍然复用前驱的倒排（复用数 1，倒排表读取数大于 0），并可用 U+FFFD 检索到 | — | 依赖承担 | 同上 |
| tgrep-core/tests/generations.rs::sha256_repositories_and_unicode_paths_are_supported | SHA-256 对象格式的仓库可以建 generation：tree、commit、blob 的 id 都是 64 位十六进制；路径含非 ASCII 字符（`source-λ.txt`，仓库目录名 `repository λ`）可以检索 | — | 不保留 | git-strata v1 不支持 SHA-256 仓库：`TreeOid` 与 `CommitOid` 只接受 40 位，遇到时返回 `Code::Unsupported`（types.md §3 与 §6.2），与本用例相反；gitio.md 须有一条用例证明 SHA-256 仓库被明确拒绝。非 ASCII 路径的往返由 `RepoPath`（UTF-8、NFC）的用例承担 |
| tgrep-core/tests/generations.rs::non_unicode_repository_roots_are_lossless_but_tracked_paths_are_unsupported | 仓库根目录名含非 UTF-8 字节：macOS 上 APFS 在创建时就拒绝（errno 92），`Repository::discover` 返回 I/O 错误并提前结束；其他 Unix 上根目录无损往返，但 tree 里出现非 UTF-8 路径时 `ensure` 报 `Unsupported` | types | 改写 | macOS 上只会走到 APFS 拒绝的分支。tree 或 index 里来自其他平台的非 UTF-8 路径，在 git-strata 里是 `Code::NotText`（types.md §6.2：既成不了 `RepoPath`，也写不到 APFS 上），必须明确报错而不是跳过，由 types 的 `RepoPath` 构造与 gitio 的 tree 读取用例承担 |
| tgrep-core/tests/generations.rs::read_only_publication_lock_returns_permission_error | 发布锁文件被设为只读时，`ensure` 返回 `PermissionDenied` | — | 不保留 | `#[cfg(windows)]`，macOS 上不编译：Windows 上只读文件锁的行为；git-strata 不用 tgrep 的发布锁文件 |
| tgrep-core/tests/managed_catalog_windows.rs::native_rendezvous_records_timeout | 测试用的会合点（`Rendezvous`）超时后记下超时，再次等待仍保持这一记录 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖；该用例仅 Windows 编译，macOS 上不运行 |
| tgrep-core/tests/managed_catalog_windows.rs::canonical_windows_catalog_readers_release_native_wal_locks | Windows 上以规范化路径打开的两个并发 SQLite 读者结束并关闭后，WAL 的原生共享锁都已释放，`wal_checkpoint(TRUNCATE)` 能清空 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖；该用例仅 Windows 编译，macOS 上不运行 |
| tgrep-core/tests/managed_catalog_windows.rs::concurrent_readmark_initialization_is_not_native_reader_overlap | 两个读者并发初始化 WAL read-mark 时只算一次原生锁调用，不被误判为两个读者重叠，checkpoint 同样能清空 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖；该用例仅 Windows 编译，macOS 上不运行 |
| tgrep-core/tests/managed_lifecycle.rs::superseded_reconciliation_preserves_ready_queries_and_exact_checkpoint | 被更新的对账取代时，已就绪视图的查询保持可用，确切的 checkpoint 保持不变 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::ready_reconciliation_preserves_forced_repairs_and_rejects_invalid_work | 已就绪视图上的对账保留强制修复（漏掉通知的改动与未跟踪文件仍被找出），并拒绝无效的工作请求 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::coalesced_reconciliation_rechecks_cancellation_and_allocation_before_receipt | 合并的对账在出回执之前复查取消与配额分配（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::metadata_walk_reserves_directory_queue_before_retaining_a_wide_tree | 元数据遍历在保留一棵很宽的目录树之前，先为目录队列预留配额 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::repeated_blob_extraction_uses_table_capacity_not_a_whole_blob_worst_case | 重复内容的 blob 提取按表容量计费，而不是按整个 blob 的最坏情况 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::retained_memory_follows_escaped_readers_without_retaining_work_slots | 逃逸出去的读者持有的内存继续计费，但不占用工作槽位 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::atomic_migration_preserves_dirty_overlay_old_queries_and_exact_replay | 视图原子迁移到新基线时，脏 overlay、旧查询与确切重放都保持不变（新提交分支、脏替换与未跟踪内容各自可检索） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::fixed_pin_participant_blocks_migration_until_its_independent_detach | 固定钉住的参与者阻止迁移，直到它独立脱离 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::pending_inputs_do_not_exhaust_adaptive_work_before_reconciliation | 尚未对账的待处理输入不会在对账之前耗尽自适应工作的额度 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::deferred_adaptive_migration_retains_its_target_and_single_attempt_budget | 被推迟的自适应迁移保留其目标与单次尝试的额度 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::changed_allocation_rearms_exhausted_adaptive_work_without_changing_the_target | 分配变化会重新武装已耗尽的自适应工作，但不改变迁移目标（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::adaptive_advancement_uses_own_head_and_rejects_untracked_or_transformed_cost | 自适应推进只用视图自己的 HEAD，不把未跟踪或经过转换（smudge）的内容计入成本 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::managed_publication_reuses_one_guarded_generation | 受管发布复用同一个受守卫保护的 generation | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::transient_retirement_locks_defer_attachment_without_changing_its_exact_intent | 短暂的退役锁使 attach 推迟，但不改变它的确切意图 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::unknown_missing_replaced_and_inaccessible_owner_proofs_never_release_leases | 所有者证明未知、缺失、被替换或不可访问时，永不释放租约 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::managed_format_and_escaped_reader_cannot_bypass_ownership | 受管格式的 generation 不能被普通 `IndexReader` 或 `SharedBase` 打开，把各段改名成旧文件名复制出去也不行；逃逸出去的原始 reader 仍保护它的物理对象，不能绕过所有权 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::unrelated_store_named_directories_keep_ordinary_indexes_usable | 名字与受管 store 相同的无关目录不影响普通索引与旧式 `GenerationManager`；受管保留目录则拒绝普通的写入与打开 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::staging_exhaustion_is_typed_and_never_published | 暂存空间耗尽返回带类型的错误且从不发布，部分封存按实际字节计费 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::inactive_roots_get_new_view_and_guard_incarnations_without_exhausting_slots | 不活跃的根获得新的视图与守卫世代，且不耗尽槽位 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::offline_recovery_preserves_live_owner_and_reader_proofs_after_git_deletion | git 对象被删除后，离线恢复仍保留活着的所有者与读者证明 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::idle_shutdown_distinguishes_lease_release_from_escaped_root_readers | 空闲关停区分租约释放与逃逸出去的根读者 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::namespace_allocations_are_version_checked_and_independent | namespace 的配额分配带版本检查且相互独立 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::managed_checkpoint_is_delta_only_bound_and_restored_not_ready | 受管 checkpoint 只存增量、绑定到基线，还原后视图处于未就绪状态；陈旧的绑定必须失败 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::escaped_candidate_keeps_the_base_and_root_protected | 逃逸出去的候选者使基线与根继续受保护 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::checkpoint_withdrawal_precedes_generation_collection_without_stopping_readers | 回收 generation 之前先撤回 checkpoint，且不停止读者 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::retained_checkpoints_release_independent_edges_and_stale_preview_cannot_authorize_deletion | 保留的 checkpoint 各自独立地释放引用边；过期的预览不能授权删除 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::native_sharing_and_mapping_failures_stay_pending_without_false_reclaimed_bytes | Windows 原生的共享冲突与映射失败使对象保持待处理，不虚报已回收的字节 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖；该用例仅 Windows 编译，macOS 上不运行 |
| tgrep-core/tests/managed_lifecycle.rs::native_sharing_recovery_resumes_a_deadline_limited_collection_pass | Windows 原生共享恢复后，被时限截断的回收遍历得以继续（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖；该用例仅 Windows 编译，macOS 上不运行 |
| tgrep-core/tests/managed_lifecycle.rs::disk_full_during_member_creation_preserves_the_prior_ready_pin_and_conservative_charges | 创建成员时磁盘写满：保留先前就绪的钉住，并按保守口径计费（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::catalog_errors_do_not_leave_a_busy_idle_stop_with_closed_admission | 目录库出错不会留下『忙碌的空闲停止』与已关闭的准入（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::atomic_idle_admission_rejects_new_owners_operations_and_independent_readers | 原子的空闲准入拒绝新的所有者、新操作与独立读者；缓存引用不算活跃工作（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::controlled_external_sort_spills_and_preserves_shared_blob_postings | 受控的外部排序溢出到磁盘时，共享 blob 的倒排不丢文件 | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::generation_retirement_races_rebuild_and_attach_without_aliasing_incarnations | generation 退役与重建、attach 竞争时，不同世代不会混为同一个（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::checkpoint_retirement_races_restore_save_and_predecessor_build_without_partial_opens | checkpoint 退役与还原、保存、前驱构建竞争时，不出现半开状态（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::intermediate_publication_failures_do_not_commit_a_view_migration | 发布的中间步骤失败时不提交视图迁移（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::view_commit_boundaries_preserve_exact_authority_and_replay | 视图提交边界保持确切的授权与重放（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::first_attach_and_same_version_refresh_recover_their_committed_publications | 首次 attach 与同版本刷新能恢复各自已提交的发布（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_lifecycle.rs::unlink_before_credit_is_recovered_once_and_cannot_remove_a_new_incarnation | 先 unlink 后入账的情形只恢复一次，且不会误删新的世代（需 `managed-test-hooks` 特性） | — | 不保留 | git-strata 不调用 tgrep managed v2；由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/managed_storage_macos.rs::native_content_generation_invalidates_writes_and_rejects_retained_writable_mappings | 直接用 `fgetattrlist` 读 macOS 的文件内容代数（`ATTR_CMN_GEN_COUNT`，非零）：写入后代数改变，即使事后把访问时间与修改时间恢复成原值也改变；对文件建立可写内存映射期间代数取不到（映射后、经映射写入后、flush 后三个时点都是空），不能当作可复用的内容证据；解除映射后代数不同于写入后的值，且读到经映射写入的字节 | apfs | 保留 | 文件名虽带 managed，用例却不调用任何 tgrep API，是对 macOS 行为的直接探针，正是 apfs 判变所依赖的原语（设计 §5 与 §6.2：用 gen count 对比基线清单）：对应 apfs.md §7 `gen_count_changes_on_write_not_on_touch` 的『写入改变』一半（事后恢复时间戳也掩盖不了写入）；『只 touch 不改变』的另一半本例没有测，apfs 的用例要补。新增要求：文件上存在可写映射时代数取不到，apfs 须把它当『未知』并改用内容哈希，不得当作未改动 |
| tgrep-core/tests/shared_worktrees.rs::worktrees_share_reader_but_not_changes_or_roots | 两个 worktree 从同一个基线索引建出后共享同一个 reader（`Arc::ptr_eq`），但改动、新增、删除与根目录各自独立：A 的改动与新增在 B 里看不到，B 的改动在 A 里看不到，A 删掉的基线文件在 B 里仍在，未改动的第三个视图仍看到完整的基线 | search | 改写 | 对应 search.md §7 `base_index_shared_across_sessions_same_tree`（同 tree 的会话共用一份基线索引）、`overlay_entry_shadows_base_path` 与 `tombstone_hides_base_in_this_session_only`；worktree 换成会话工作区，`live.upsert_file` 与 `delete_file` 换成 overlay 条目与墓碑 |
| tgrep-core/tests/shared_worktrees.rs::checkpoint_preserves_masks_tombstones_empty_files_and_unmasked_postings | 保存 overlay 再还原后，对多种模式（含 `.` 与短模式）与两种大小写模式的查询答案与保存前完全一致：替换、短文件、空文件、不带掩码的倒排、墓碑（包括对基线里本就不存在的路径记的墓碑）都被保留，掩码逐项相同，还原后仍共享同一个 reader | search | 改写 | 保留的是『重建前后查询答案一致』：git-strata 的 checkpoint 存内容寻址的 blob 与清单，不存 tgrep 的 overlay 索引 JSON，search 的每会话 overlay 索引须能由 overlay 条目与工作区重建，并且对 `.`、短文件、空文件、墓碑的答案不变。掩码逐项比较属索引内部格式，不保留；『对不存在的路径记墓碑』也不沿用（未跟踪文件被删只是消失） |
| tgrep-core/tests/shared_worktrees.rs::checkpoint_contains_only_overlay_and_never_changes_base_files | checkpoint 里只有改动过的条目（未改动的基线文件不出现）；同一状态连存两次得到逐字节相同的文件；基线索引的四个文件逐字节不变；目录里没有多余的文件 | overlay | 改写 | 对应设计 §6.2 与 §8 的 checkpoint = O(改动文件数)，以及不变量 5（基线 generation 永不原地修改）：清单只列改动的路径，保存不碰基线，同一状态再存一次不产生新内容（blob 按内容寻址去重）；『逐字节相同的 JSON 文件』换成『同一组 blob』 |
| tgrep-core/tests/shared_worktrees.rs::repeated_saves_replace_existing_checkpoint_with_latest_overlay | 同一个 checkpoint 路径反复保存，后一次替换前一次：还原得到的是最新的 overlay（旧修订的内容不再命中，新增与删除都在），全部删光后还原出 0 个文件并保留各路径的墓碑，目录里始终只有一个文件 | — | 不保留 | tgrep 的 checkpoint 是每个 worktree 一个文件、后存覆盖前存；git-strata 的 checkpoint 只追加（CheckpointId 递增，旧的仍可还原，设计 §6.2），还原某个 checkpoint 得到的是它保存时的状态，由 overlay.md §7 的 checkpoint 与 restore 用例（含 `restore_is_itself_checkpointed`）承担 |
| tgrep-core/tests/shared_worktrees.rs::locked_checkpoint_preserves_previous_save_and_allows_retry | checkpoint 文件被独占锁住时，再次保存失败，上一次的 checkpoint 字节不变、目录里没有残留文件、仍可还原；锁释放后重试成功 | — | 不保留 | `#[cfg(windows)]`：靠 Windows 的独占共享模式制造写失败，macOS 上不编译，git-strata 只支持 macOS。『保存失败时上一个 checkpoint 完好、无残留、可重试』由 `checkpoint_cannot_overwrite_base_or_leave_temporary_files_after_failure` 一行承接 |
| tgrep-core/tests/shared_worktrees.rs::restored_changes_can_be_reverted_to_base_without_affecting_other_worktrees | 从 checkpoint 还原出的 overlay 里，把『改动过』与『已删除』的路径清掉后重新看到基线内容（`original` 同时命中两个文件），原来的 worktree 实例不受影响；清空后再保存、再还原，没有待处理的改动，文件数回到 2 | overlay | 改写 | 对应 overlay.md §7 `restored_to_base_removes_override`（改回与基线相同则覆盖消失）与 `deleted_base_file_is_tombstone`（删基线文件记墓碑，文件放回则墓碑消失）；『其他 worktree 不受影响』即会话之间的 overlay 互不可见 |
| tgrep-core/tests/shared_worktrees.rs::identical_base_at_another_location_can_restore_checkpoint | 把基线索引的四个文件复制到另一个目录，用副本打开的共享基线仍能还原原来保存的 checkpoint（基线身份由内容决定，与所在目录无关），被删除的基线文件仍是墓碑，文件数为 0 | overlay | 改写 | checkpoint 对基线的引用是内容身份而不是目录路径：git-strata 里是 `BaseGen`（仓库、tree OID、依赖集，types.md §4），基线目录被搬动或重建后，同一 `BaseGen` 上的 checkpoint 仍可还原；tgrep 『对索引字节求身份』的做法不沿用 |
| tgrep-core/tests/shared_worktrees.rs::checkpoint_rejects_changed_postings_even_with_identical_path_table | 路径表完全相同、但倒排内容不同（`abcdef` 换成 `uvwxyz`）的基线不能还原这个 checkpoint，报『不同的基线快照』 | overlay | 改写 | checkpoint 只能还原到它记录的 `BaseGen`：文件路径相同而内容不同的 tree 有不同的 OID，必须拒绝（或按发布后 rebase 的算法投影到新基线，设计 §6.3）；git-strata 用 tree OID 判同一性，不比较索引字节 |
| tgrep-core/tests/shared_worktrees.rs::checkpoint_identity_covers_masks_not_just_trigram_membership | 基线身份连位置与后继掩码也一并计入：把 index.bin 里一个掩码字节改成 0xFF 的副本不能还原原 checkpoint，同样报『不同的基线快照』 | — | 不保留 | 基线身份在 git-strata 是 tree OID 与依赖集，不由索引字节的哈希决定；loc 与 next 掩码属 tgrep 索引格式的内部细节 |
| tgrep-core/tests/shared_worktrees.rs::wrong_root_and_reader_swap_are_rejected | 把 checkpoint 还原到另一个根目录被拒绝（报『不同的根』），用 `root/.` 写法的同一目录则接受；worktree 的 reader 被换掉后再保存被拒绝，旧 checkpoint 字节不变，兄弟 worktree 不受影响 | overlay | 改写 | checkpoint 属于一个会话工作区：还原到另一个会话的目录必须拒绝，同一目录的不同写法（带 `.`）要认作同一个；会话的基线被 `session::move_workspace` 换掉之后，对旧基线保存 checkpoint 必须被拒绝，不得写出新旧混合的状态 |
| tgrep-core/tests/shared_worktrees.rs::malformed_checkpoints_fail_instead_of_revealing_base_entries | 损坏或非法的 checkpoint 一律还原失败：版本号不对；路径是 `..`、绝对路径、`a//b`、`a/./b`、含反斜杠或空；墓碑指向非法路径、与文件条目互相矛盾或重复；文件条目重复；trigram 越界、掩码为 0 或重复；缺 `deleted` 字段；根目录编码非法；JSON 被截断；文件不存在。失败后不会退化成只还原一部分、让被删的基线文件重新出现 | overlay | 改写 | 对应不变量 6（不确定的失败保留现场并报告）：还原遇到损坏的清单整体失败，不得悄悄丢掉墓碑；路径校验对应 types.md §3 的 `RepoPath` 构造（`..`、绝对路径、空段、`.` 都是 `InvalidInput`）；JSON 字段、trigram 范围与根目录编码这几类是 tgrep 文件格式的细节，不保留 |
| tgrep-core/tests/shared_worktrees.rs::invalid_live_paths_fail_before_replacing_a_checkpoint | overlay 里混入非法路径（`../outside.rs`）时保存失败，上一个 checkpoint 原样保留，目录里没有多出文件 | overlay | 改写 | checkpoint 先校验后提交：含非法路径的状态不产生半个 checkpoint，上一个 checkpoint 不受影响（元数据在 SQLite 里，一次事务提交，设计 §7）；非法路径在 git-strata 里更早就构造不出 `RepoPath` |
| tgrep-core/tests/shared_worktrees.rs::checkpoint_cannot_overwrite_base_or_leave_temporary_files_after_failure | 把 checkpoint 存到基线自己的 `index.bin`、不存在的目录下或一个目录路径上都失败，基线字节不变，目录里没有临时文件，worktree 仍可正常查询 | overlay | 改写 | 对应不变量 5 与 6：checkpoint 的写入目标只有 store（blob 与元数据行），无法指向基线 generation，失败不留半截状态；tgrep 里『调用方指定任意目标文件』的情形在 git-strata 里不存在，只保留『基线不被改、失败不留残渣』 |
| tgrep-core/tests/shared_worktrees.rs::incomplete_or_legacy_metadata_does_not_change_existing_open_behavior | 索引的 meta.json 标成不完整（complete 为假、hidden_complete 为假、缺 file_table_id、版本号更高）时，共享基线的打开被拒绝，而旧式的 `HybridIndex::open` 仍照常打开并答对 | — | 不保留 | 这是 tgrep 对旧版索引（没有 `file_table_id`、`hidden_complete` 等元数据）的兼容路径；git-strata 的基线索引都是自己新建的，没有旧版索引要兼容，也没有两种打开方式并存。『未完成的索引不当共享基线用』由 `SharedBase::open` 的拒绝承担，由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/shared_worktrees.rs::directory_shaped_checkpoint_paths_do_not_replace_existing_files | checkpoint 的目标路径写成目录形式（末尾加 `/`、`/.`、`/./`，Windows 上还有反斜杠形式）时保存被拒绝，已有的 checkpoint 文件不被替换 | — | 不保留 | tgrep 的 checkpoint 是调用方指定的文件路径，需要防目录写法覆盖已有文件；git-strata 的 checkpoint 是 store 里的行，没有调用方指定的目标路径 |
| tgrep-core/tests/shared_worktrees.rs::shared_base_rejects_unaligned_overlapping_and_gapped_posting_ranges | lookup.bin 里某个条目的偏移被改成不对齐、与前一条重叠或留下空洞时，`SharedBase::open` 拒绝，而普通 `IndexReader` 仍可打开且 `validate_lookup` 通过 | — | 依赖承担 | search 经库的 `SharedBase::open` 打开自己建的基线索引，打开前的格式校验由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/shared_worktrees.rs::shared_base_rejects_unreferenced_posting_bytes | index.bin 末尾多出 1 字节或 6 字节无人引用的数据时，`SharedBase::open` 拒绝 | — | 依赖承担 | 同上 |
| tgrep-core/tests/shared_worktrees.rs::shared_base_rejects_invalid_posting_ids_and_location_masks | 倒排条目的文件号越界（等于文件数或为 u32::MAX）或位置掩码为 0 时拒绝，首条与末条两个位置各试一次，而普通 `IndexReader` 仍可打开且校验通过 | — | 依赖承担 | 同上 |
| tgrep-core/tests/shared_worktrees.rs::shared_base_rejects_out_of_range_trigrams | lookup.bin 里的 trigram 超出 24 位（0x01000000）时拒绝 | — | 依赖承担 | 同上 |
| tgrep-core/tests/shared_worktrees.rs::shared_base_rejects_duplicate_or_descending_posting_ids | 同一个 trigram 的倒排条目文件号重复或递减（0 与 0、1 与 1、1 与 0，掩码各不相同）时拒绝，而普通 `IndexReader` 校验通过 | — | 依赖承担 | 同上 |
| tgrep-core/tests/shared_worktrees.rs::shared_base_rejects_mismatched_empty_sections_even_with_zero_metadata_count | lookup.bin 或 index.bin 之一被清空而另一段非空时拒绝，即使元数据里的 trigram 数改成 0；普通 `IndexReader` 保持原先对空段的行为（trigram 数为 0） | — | 依赖承担 | 同上 |
| tgrep-core/tests/shared_worktrees.rs::shared_base_rejects_truncated_sections_with_stale_metadata | lookup.bin 少了最后一条而元数据计数没跟着改、或两段全空而元数据计数非零时拒绝 | — | 依赖承担 | 同上 |
| tgrep-core/tests/shared_worktrees.rs::shared_base_rejects_inconsistent_file_and_trigram_counts | meta.json 里的文件数或 trigram 数被改成 0、多 1 或 u64::MAX 时拒绝，改回原值后可以打开 | — | 依赖承担 | 同上 |
| tgrep-core/tests/shared_worktrees.rs::shared_base_accepts_empty_sections_for_empty_and_short_files | 只含空文件与 2 字节文件的基线没有任何 trigram，`SharedBase` 仍可打开，`.`（MatchAll）列出两个文件，checkpoint 往返后文件数仍为 2 | — | 依赖承担 | 边界：tree 里只有空文件或短文件时，基线索引的各段为空但合法，search 打开并列出成员走库路径，由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/shared_worktrees.rs::unicode_root_checkpoint_retains_legacy_string_representation | 根目录是合法 Unicode 时，checkpoint 里的 root 字段仍用旧版的字符串表示（version 为 1，值为规范化后的路径），可按 `PathBuf` 反序列化，并可还原 | — | 不保留 | 这是 tgrep checkpoint JSON 的向后兼容格式；git-strata 没有旧版的 checkpoint 文件，checkpoint 存在 store 里 |
| tgrep-core/tests/shared_worktrees.rs::non_unicode_roots_roundtrip_without_lossy_identity_collisions | 根目录名含非 UTF-8 字节（Unix）或孤立代理项（Windows）时，两个仅在无效字节上不同的根经有损转换后相同，但 checkpoint 用无损编码区分它们，不会把一个根的 checkpoint 还原到另一个根 | — | 不保留 | `#[cfg(any(target_os = "linux", windows))]`，macOS 上不编译：APFS 本身拒绝非 UTF-8 文件名（测试内的注释也这么说）；git-strata 只支持 macOS，根路径一律是 UTF-8。git tree 里来自别的平台的非 UTF-8 路径按 types.md §6.2 返回 `Code::NotText` |
| tgrep-core/tests/shared_worktrees.rs::shared_base_rejects_unsafe_or_duplicate_paths | 基线索引的路径表含 `../outside.rs` 或重复的 `same.rs` 时，`SharedBase::open` 拒绝 | — | 依赖承担 | 路径表校验在库的 `SharedBase::open` 里，由 tgrep-core 自身测试覆盖；git-strata 的路径来自 git tree，`RepoPath` 的构造已拒绝 `..` 与空段，索引层是第二道防线 |
| tgrep-core/tests/shared_worktrees.rs::ordinary_builder_produces_a_compatible_shared_base | `builder::build_index` 建出的普通索引（含 `.hidden.rs` 这样的点文件）可被 `SharedBase::open` 打开，从中建出的 worktree 按内容找到两个文件 | — | 依赖承担 | search 用库的 builder 建基线索引、再用 `SharedBase` 打开，这条往返路径由 tgrep-core 自身测试覆盖；点文件与被 ignore 的已跟踪文件是否进基线，按 tree 的内容定（见 `tgrep-core/tests/generations.rs::builds_committed_blobs_not_staged_dirty_or_untracked_contents` 一行） |
| tgrep-core/tests/shared_worktrees.rs::many_worktrees_share_one_reader_while_querying_independent_overlays | 8 个线程各自从同一个基线建 worktree，全部共享同一个 reader（`Arc::ptr_eq`），各自改写 `base.rs` 后只在自己的视图里命中私有内容、看不到基线原文，保存再还原 checkpoint 后仍共享同一个 reader | search | 改写 | 对应 search.md §7 `base_index_shared_across_sessions_same_tree`：daemon 并发服务多个会话，同一 tree 的基线索引只有一份，各会话的 overlay 互不可见（`tombstone_hides_base_in_this_session_only`） |
| tgrep-core/tests/shared_worktrees.rs::worktree_root_must_exist_and_be_a_directory | 为不存在的路径或普通文件建 worktree 会报错；对空 overlay 保存再还原，得到 0 个文件、没有待处理的改动 | — | 依赖承担 | search 经库的 `create_worktree` 取得会话视图，要求根目录存在且是目录是库的契约，由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/snapshot_consistency.rs::candidate_totals_account_for_overlay_and_reader_snapshot | overlay 里替换已有路径、新增路径、删除基线路径、删除本就不存在的路径之后，MatchAll 命中 3 个，`num_files()` 与 `num_files_using(快照)` 都等于命中数；把 reader 换成空索引后 `num_files()` 只剩 overlay 的 2 个，而用旧快照仍是 3；之后 overlay 的增删按快照基准计数（4、3） | search | 改写 | 对应 search.md §7 `overlay_entry_shadows_base_path` 与 `tombstone_hides_base_in_this_session_only`：替换同一路径不重复计数，新增加一，墓碑减一，对不存在路径的删除是空操作；候选总数须与查询所用的同一份（基线、overlay）快照一致，不得混用换基线前后的两份 |
| tgrep-core/tests/snapshot_consistency.rs::all_query_ids_resolve_via_snapshot | 查询返回的每个文件号，都能用同时返回的 reader 快照解析成相对路径与完整路径（没有并发换基线） | — | 依赖承担 | search 调库的 `execute_query_with_masks` 与 `resolve_path` / `resolve_full_path`，命中必能解析是库的契约，由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/snapshot_consistency.rs::snapshot_survives_reader_swap | 把 reader 换成一份完全不同的索引之后，换基线前查询得到的文件号仍能用当时返回的快照解析；用换基线后的当前 reader 解析旧文件号会失败（新索引的文件更少） | search | 改写 | 会话的基线被 `session::move_workspace` 换掉时，正在进行的 grep 须继续用它开始时的快照解析命中，不得拿新基线去解析旧文件号而丢失或错配命中；search 须沿用『查询返回快照、再用快照解析』的接口，不得再用『现取当前 reader』的 `file_path(id)` |
| tgrep-core/tests/snapshot_consistency.rs::new_query_after_swap_resolves_correctly | 换基线之后的新查询只看新索引（命中 2 个新文件），文件号能用新快照解析，没有旧状态残留 | search | 改写 | 同上：`move_workspace` 之后，该会话的新 grep 只面对新基线（`BaseGen`），不残留旧基线的命中 |
| tgrep-core/tests/snapshot_consistency.rs::match_all_uses_consistent_snapshot | 单字符正则（`.`）退化成 MatchAll 计划，返回全部 2 个文件，并同样带回可解析它们的快照 | — | 依赖承担 | 没有三元组可用的模式走库的 MatchAll 路径，search 直接调用，由 tgrep-core 自身测试覆盖 |
| tgrep-core/tests/snapshot_consistency.rs::direct_lookups_share_snapshot_overlay_precedence | 单个三元组的直接查找也遵循 overlay 优先：被替换的文件按新内容、被删除的文件不出现、新增的文件出现，文件号顺序是基线在前、overlay 在后；`all_file_ids` 与 MatchAll 一致；编码后的条目与快照里的一致 | search | 改写 | 对应 search.md §7 `overlay_entry_shadows_base_path`：overlay 条目遮住同路径的基线条目，墓碑隐藏基线条目（`HybridIndex` 的精确语义，设计 §6.3）；直接查找与查询计划走同一套遮蔽。文件号的排布与条目编码是内部细节，不保留 |
| tgrep-core/tests/snapshot_consistency.rs::concurrent_swap_during_query | 一个线程拿到查询结果与快照后，另一个线程把 reader 换成别的索引，前一个线程再用快照解析全部文件号，全部成功 | search | 改写 | 同 `snapshot_survives_reader_swap`，再加并发：daemon 在服务 grep 的同时可能移动会话的基线，解析必须全部基于查询时的快照；search.md 须写明这一不变量并给并发用例 |

总计: 635
