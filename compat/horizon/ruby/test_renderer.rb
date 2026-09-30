# frozen_string_literal: true

require 'json'
require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
$LOAD_PATH.unshift(File.join(ENV.fetch('LIQUID_RUBY_ROOT'), 'lib'))
require_relative 'renderer'

class HorizonFixtureRendererTest < Minitest::Test
  def setup
    @fixture = JSON.parse(File.read(ENV.fetch('HORIZON_STORE', File.expand_path('../store.json', __dir__))))
    @theme = ENV.fetch('HORIZON_THEME_ROOT')
  end

  def renderer
    HorizonFixture::Renderer.new(theme_root: @theme, fixture: @fixture)
  end

  def test_real_hero_renders_ordered_text_and_button_with_global_block_context
    host = renderer
    result = host.render(scope: 'hero')
    assert_includes result, '<section id="shopify-section-hero_jVaWmY" class="shopify-section hero-wrapper section-wrapper">'
    assert_includes result, 'Equip your everyday adventure'
    assert_includes result, 'Shop field essentials'
    assert_includes result, 'href="/collections/all"'
    assert_operator result.index('Equip your everyday adventure'), :<, result.index('Shop field essentials')
    assert_equal 1, result.scan('aria-label="Fixture placeholder"').size
    refute_includes result, 'Liquid error'
    refute_empty host.stylesheets
    assert_includes host.rendered_sources, 'snippets/button.liquid'
  end

  def test_real_product_list_uses_shared_store_products_and_prices
    result = renderer.render(scope: 'template')
    @fixture.fetch('globals').fetch('all_products').each_value do |product|
      assert_includes result, product.fetch('title')
      assert_includes result, product.fetch('url')
    end
    assert_includes result, '$18.00'
    assert_includes result, '$24.00'
    assert_includes result, '$32.00'
    assert_includes result, '--focal-point: 50.0% 50.0%;'
    assert_includes result, 'data-testid="product-list"'
    refute_includes result, '{{ closest.collection.title }}'
  end

  def test_repeated_render_does_not_leak_state_or_mutate_store
    host = renderer
    original = JSON.generate(@fixture)
    first = host.render(scope: 'template')
    first_css = host.stylesheets.dup
    first_sources = host.rendered_sources.dup
    assert_equal first, host.render(scope: 'template')
    assert_equal first_css, host.stylesheets
    assert_equal first_sources, host.rendered_sources
    assert_equal original, JSON.generate(@fixture)
    host.render(scope: 'hero')
    assert_equal first, host.render(scope: 'template')
  end

  def test_full_page_renders_layout_header_footer_and_nonempty_cart_deterministically
    host = renderer
    original = JSON.generate(@fixture)
    result = host.render(scope: 'page')
    assert_match(/\A<!doctype html>/, result)
    assert_match(/<title>\s*Harbor Supply<\/title>/, result)
    assert_includes result, %(href="#{@fixture.dig('globals', 'canonical_url')}")
    assert_includes result, 'id="header-component"'
    assert_includes result, 'id="MainContent"'
    assert_includes result, 'id="shopify-section-footer"'
    assert_includes result, '&copy; 2026'
    assert_includes result, @fixture.dig('globals', 'powered_by_link')
    assert_includes result, '<bdi>$131.00 USD</bdi>'
    assert_includes result, 'data-cart-line="1"'
    assert_includes result, 'data-cart-line="2"'
    assert_equal 1, result.scan('<style data-horizon-fixture>').size
    refute_includes result, HorizonFixture::Renderer::CSS_SENTINEL
    assert_includes result, "<style data-horizon-fixture>#{host.stylesheets.values.join}</style>"
    assert_operator result.index('<style data-horizon-fixture>'), :<, result.index('</head>')
    %w[layout/theme.liquid sections/header.liquid sections/footer.liquid snippets/cart-items-component.liquid].each do |source|
      assert_includes host.rendered_sources, source
    end
    css = host.stylesheets.dup
    assert_equal result, host.render(scope: 'page')
    assert_equal css, host.stylesheets
    assert_equal original, JSON.generate(@fixture)
  end

  def test_payment_terms_requires_explicit_disabled_platform_service
    @fixture.fetch('manifest').fetch('platform_capabilities')['payment_terms'] = true
    error = assert_raises(HorizonFixture::ContractError) { renderer.render(scope: 'page') }
    assert_includes error.message, 'payment_terms'
  end

  def test_nested_render_keeps_unknown_filter_errors
    with_micro_theme(block_source: "{% render 'leaf' %}{% schema %}{\"tag\":null}{% endschema %}", snippet: '{{ block.settings.text | unsupported_fixture_filter }}') do |host|
      assert_raises(Liquid::UndefinedFilter) { host.render(scope: 'hero') }
    end
  end

  def test_siblings_keep_global_block_section_and_local_assigns_isolated
    block = "{% assign leaked = block.settings.text %}{% render 'leaf' %}{% schema %}{\"tag\":null}{% endschema %}"
    with_micro_theme(block_source: block, snippet: '{{ section.id }}:{{ block.id }}:{{ block.settings.text }}:{{ leaked }};') do |host|
      expected = '<div id="shopify-section-test" class="shopify-section">test:second:B:;test:first:A:;</div>'
      assert_equal expected, host.render(scope: 'hero')
      assert_equal expected, host.render(scope: 'hero')
    end
  end

  def test_section_blocks_are_an_ordered_array_with_string_ids
    block = "{% render 'leaf' %}{% schema %}{\"tag\":null}{% endschema %}"
    with_micro_theme(block_source: block, snippet: "{{ section.blocks | find_index: 'id', block.id }};") do |host|
      assert_equal '<div id="shopify-section-test" class="shopify-section">0;1;</div>', host.render(scope: 'hero')
    end
    blocks = HorizonFixture::BlockCollection.new({ 'static' => { 'static' => true }, 'a' => {} }, ['a'])
    assert_equal %w[a static], blocks.map { |node| node.fetch('id') }
    assert_equal({ 'static' => true }, blocks.fetch('static'))
    assert_equal 'a', blocks.fetch(0).fetch('id')
    assert_raises(KeyError) { blocks.fetch('missing') }
  end

  def test_static_stylesheet_is_not_evaluated_and_is_collected_once
    block = "{% stylesheet %}.test { content: '{{ untouched }}'; }{% endstylesheet %}{% render 'leaf' %}{% schema %}{\"tag\":null}{% endschema %}"
    with_micro_theme(block_source: block, snippet: '{{ block.settings.text }}') do |host|
      assert_equal '<div id="shopify-section-test" class="shopify-section">BA</div>', host.render(scope: 'hero')
      assert_equal [".test { content: '{{ untouched }}'; }"], host.stylesheets.values
    end
  end

  def test_unknown_tag_is_not_ignored
    with_micro_theme(block_source: '{% unsupported_fixture_tag %}', snippet: '') do |host|
      assert_raises(Liquid::SyntaxError) { host.render(scope: 'hero') }
    end
  end

  def test_color_preserves_hex_rendering_and_alpha_properties
    color = HorizonFixture::Color.new('#12121266')
    assert_equal '#12121266', color.to_s
    assert_equal '18 18 18', color.rgb
    assert_in_delta 0.4, color.alpha
    assert_equal '18 18 18 / 0.4', color.rgba
    assert_equal '#fbe2f3', HorizonFixture::Color.new('#EA5AB9').shifted_lightness(30)
    assert_raises(HorizonFixture::ContractError) { HorizonFixture::Color.new('var(--arbitrary)') }
    assert_raises(HorizonFixture::ContractError) { HorizonFixture::Color.new('rgba(256, 0, 0, 1)') }
  end

  def test_local_settings_bindings_keep_theme_settings_visible
    host = renderer
    values = { 'local' => '{{ settings.theme_value }}', 'theme_value' => 'wrong' }
    materialized = host.send(:materialize_settings, values, [], 'settings' => { 'theme_value' => 'theme' })
    assert_equal 'theme', materialized.fetch('local')
    initial = host.send(:materialize_settings, values, [], {})
    assert_equal 'wrong', initial.fetch('local')
  end

  def test_source_rejects_snippet_path_traversal
    source = HorizonFixture::Source.new(@theme)
    assert_raises(HorizonFixture::ContractError) { source.read_template_file('../layout/theme') }
  end

  private

  def with_micro_theme(block_source:, snippet:)
    Dir.mktmpdir('horizon-contract') do |directory|
      files = {
        'config/settings_schema.json' => '[]',
        'config/settings_data.json' => '{"current":{}}',
        'sections/sample.liquid' => "{% content_for 'blocks' %}{% schema %}{\"settings\":[]}{% endschema %}",
        'blocks/sample.liquid' => block_source,
        'snippets/leaf.liquid' => snippet,
        'templates/index.json' => JSON.generate('sections' => { 'test' => { 'type' => 'sample', 'blocks' => { 'first' => { 'type' => 'sample', 'settings' => { 'text' => 'A' } }, 'second' => { 'type' => 'sample', 'settings' => { 'text' => 'B' } } }, 'block_order' => %w[second first] } }, 'order' => ['test'])
      }
      files.each do |path, body|
        absolute = File.join(directory, path)
        FileUtils.mkdir_p(File.dirname(absolute))
        File.write(absolute, body)
      end
      fixture = { 'schema_version' => 1, 'synthetic' => true, 'globals' => {}, 'theme' => {}, 'pages' => { 'index' => { 'template' => 'templates/index.json', 'title' => 'Test', 'description' => 'Test', 'url' => 'https://test.example/' } } }
      yield HorizonFixture::Renderer.new(theme_root: directory, fixture: fixture)
    end
  end
end
