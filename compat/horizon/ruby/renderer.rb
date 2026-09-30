# frozen_string_literal: true

require 'cgi'
require 'digest'
require 'json'
require 'liquid'
require 'pathname'

# A deterministic fixture host for pinned Horizon sources. These platform adapters
# describe our fake store contract; their output is not an oracle for Shopify's server.
module HorizonFixture
  class ContractError < Liquid::Error; end

  # Liquid sees an ordered array; content_for can still retrieve original nodes by ID.
  class BlockCollection < Array
    def initialize(nodes, order)
      @nodes = nodes
      ids = order + nodes.keys.reject { |id| order.include?(id) }
      super(ids.map { |id| nodes.fetch(id).merge('id' => id) })
    end

    def fetch(index, *args, &block)
      index.is_a?(String) ? @nodes.fetch(index, *args, &block) : super
    end
  end

  class Context < Liquid::Context
    def new_isolated_subcontext
      super.tap do |child|
        child.strict_filters = strict_filters
        child.strict_variables = strict_variables
      end
    end
  end

  class Color < Liquid::Drop
    attr_reader :red, :green, :blue, :alpha

    def initialize(value)
      @value = value.to_s
      case @value
      when /\A#([\da-f]{6})([\da-f]{2})?\z/i
        digits, alpha = Regexp.last_match(1), Regexp.last_match(2)
        @red, @green, @blue = digits.scan(/../).map { |n| n.to_i(16) }
        @alpha = alpha ? alpha.to_i(16) / 255.0 : 1.0
      when /\Argba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*([\d.]+))?\)\z/
        @red, @green, @blue = (1..3).map { |i| Regexp.last_match(i).to_i }
        @alpha = Regexp.last_match(4) ? Regexp.last_match(4).to_f : 1.0
      else
        raise ContractError, "unsupported fixture color #{@value.inspect}"
      end
      raise ContractError, 'fixture color channels must be 0..255 and alpha 0..1' unless [red, green, blue].all? { |channel| (0..255).cover?(channel) } && (0..1).cover?(@alpha)
    end

    def rgb = "#{red} #{green} #{blue}"
    def rgba = "#{rgb} / #{alpha}"
    def to_s = @value
    def ==(other) = other.is_a?(Color) ? rgba == other.rgba : to_s == other

    def shifted_lightness(amount)
      r, g, b = [red, green, blue].map { |channel| channel / 255.0 }
      min, max = [r, g, b].minmax
      lightness = (min + max) / 2.0
      delta = max - min
      saturation = delta.zero? ? 0.0 : delta / (1.0 - (2.0 * lightness - 1.0).abs)
      hue = if delta.zero?
        0.0
      elsif max == r
        ((g - b) / delta) % 6
      elsif max == g
        (b - r) / delta + 2
      else
        (r - g) / delta + 4
      end
      lightness = (lightness + amount / 100.0).clamp(0.0, 1.0)
      chroma = (1 - (2 * lightness - 1).abs) * saturation
      intermediate = chroma * (1 - (hue % 2 - 1).abs)
      channels = case hue
      when 0...1 then [chroma, intermediate, 0]
      when 1...2 then [intermediate, chroma, 0]
      when 2...3 then [0, chroma, intermediate]
      when 3...4 then [0, intermediate, chroma]
      when 4...5 then [intermediate, 0, chroma]
      else [chroma, 0, intermediate]
      end.map { |channel| ((channel + lightness - chroma / 2) * 255).round }
      alpha == 1 ? format('#%02x%02x%02x', *channels) : "rgba(#{channels.join(', ')}, #{alpha})"
    end
  end

  class TemplateName < Liquid::Drop
    def initialize(name)
      @name = name
    end
    def name = @name
    def suffix = nil
    def to_s = @name
  end

  class FocalPoint < Liquid::Drop
    attr_reader :x, :y
    def initialize(value)
      @x, @y = value.fetch('x'), value.fetch('y')
    end
    def to_s = "#{x}% #{y}%"
    def to_json(*) = JSON.generate('x' => x, 'y' => y)
  end

  class Palette < Liquid::Drop
    include Enumerable
    def initialize(colors)
      @colors = colors.transform_values { |value| value.is_a?(Color) ? value : Color.new(value) }
    end
    def liquid_method_missing(name) = @colors[name]
    def each(&block) = @colors.values.each(&block)
    def size = @colors.size
    def to_a = @colors.values
  end

  # A finite local system-font stand-in, preserving configured weight and style.
  class Font < Liquid::Drop
    attr_reader :weight, :style
    def initialize(value, weight: nil, style: 'normal')
      raise ContractError, "unsupported fixture font #{value}" unless /\Ainter_n[457]\z/.match?(value)
      @value, @weight, @style = value, weight || value[-1].to_i * 100, style
    end
    def family = 'Arial'
    def fallback_families = 'sans-serif'
    def system? = true
    def to_s = @value
  end

  class Source
    def initialize(root)
      @root = Pathname(root).realpath
    end

    def read(path)
      candidate = @root.join(path).realpath
      unless candidate.to_s.start_with?("#{@root}/") && candidate.file?
        raise ContractError, "theme path escapes checkout: #{path}"
      end
      candidate.read
    rescue Errno::ENOENT
      raise ContractError, "missing theme file #{path}"
    end

    def read_template_file(name)
      unless /\A[a-zA-Z0-9_-]+(?:\/[a-zA-Z0-9_-]+)*\z/.match?(name)
        raise ContractError, "invalid snippet name #{name.inspect}"
      end
      read("snippets/#{name}.liquid")
    end

    def json(path)
      JSON.parse(read(path), allow_comments: true)
    end

    def schema(path)
      match = read(path).match(/{%-?\s*schema\s*-?%}(.*?){%-?\s*endschema\s*-?%}/m)
      match ? JSON.parse(match[1], allow_comments: true) : {}
    end
  end

  class SchemaTag < Liquid::Raw
    def render_to_output_buffer(_context, output) = output
  end

  class StylesheetTag < Liquid::Raw
    def render_to_output_buffer(context, output)
      context.registers[:horizon].collect_stylesheet(context.template_name, @body)
      output
    end
  end

  class StyleTag < Liquid::Block
    def render_to_output_buffer(context, output)
      output << '<style data-shopify>'
      super
      output << '</style>'
    end
  end

  class RenderTag < Liquid::Render
    def render_to_output_buffer(context, output)
      context.registers[:horizon].record_source("snippets/#{@template_name_expr}.liquid")
      super
    end
  end

  class ContentForTag < Liquid::Tag
    def initialize(name, markup, parse_context)
      super
      parser = parse_context.new_parser(markup)
      @kind = parse_expression(parser.expression)
      @args = {}
      while parser.consume?(:comma)
        key = parser.consume(:id)
        key += ".#{parser.consume(:id)}" if parser.consume?(:dot)
        parser.consume(:colon)
        @args[key] = parse_expression(parser.expression)
      end
      parser.consume(:end_of_string)
    end

    def render_to_output_buffer(context, output)
      args = @args.transform_values { |expr| context.evaluate(expr) }
      output << context.registers[:horizon].render_content_for(context, context.evaluate(@kind), args)
      output
    end
  end

  class SectionsTag < Liquid::Tag
    def initialize(name, markup, parse_context)
      super
      @name = parse_expression(markup.strip)
    end

    def render_to_output_buffer(context, output)
      output << context.registers[:horizon].render_group(context.evaluate(@name))
      output
    end
  end

  class PaginateTag < Liquid::Block
    def initialize(name, markup, parse_context)
      super
      parser = parse_context.new_parser(markup)
      @collection = parse_expression(parser.expression)
      raise Liquid::SyntaxError, 'paginate expects by' unless parser.id?('by')
      @size = parse_expression(parser.expression)
      parser.consume(:end_of_string)
    end

    def render_to_output_buffer(context, output)
      collection = context.evaluate(@collection)
      size = context.evaluate(@size).to_i
      raise ContractError, 'fixture pagination supports first page only, positive size required' unless size.positive?
      context.stack('paginate' => { 'current_page' => 1, 'pages' => [(collection.size.to_f / size).ceil, 1].max, 'items' => collection.size, 'page_size' => size, 'current_offset' => 0 }) do
        super
      end
    end
  end

  class FormTag < Liquid::Block
    def initialize(name, markup, parse_context)
      super
      parser = parse_context.new_parser(markup)
      @kind = parse_expression(parser.expression)
      @args = {}
      while parser.consume?(:comma)
        if parser.look(:id) && parser.look(:colon, 1)
          key = parser.consume(:id)
          parser.consume(:colon)
          @args[key] = parse_expression(parser.expression)
        else
          @resource = parse_expression(parser.expression)
        end
      end
      parser.consume(:end_of_string)
    end

    def render_to_output_buffer(context, output)
      kind = context.evaluate(@kind)
      action = { 'product' => '/cart/add', 'customer' => '/contact', 'localization' => '/localization', 'cart' => '/cart' }.fetch(kind) { raise ContractError, "unsupported fixture form #{kind}" }
      args = @args.transform_values { |expression| context.evaluate(expression) }.reject { |_, value| value.nil? }
      attrs = args.sort.map { |key, value| %( #{key}="#{CGI.escapeHTML(value.to_s)}") }.join
      output << %(<form method="post" action="#{action}"#{attrs}><input type="hidden" name="form_type" value="#{kind}">)
      context.stack('form' => { 'type' => kind, 'errors' => nil, 'posted_successfully?' => false, 'id' => args['id'] }) { super }
      output << '</form>'
    end
  end

  module Filters
    def t(key, options = {})
      host.record_filter('t')
      host.translate(key, options)
    end

    def image_url(image, options = {})
      host.record_filter('image_url')
      raise ContractError, 'fixture image_url requires an image object and width' unless image.is_a?(Hash) && image['src'] && options.keys == ['width']
      width = Integer(options.fetch('width'))
      raise ContractError, 'fixture image width must be positive' unless width.positive?
      "#{image.fetch('src')}?width=#{width}"
    end

    def image_tag(url, options = {})
      host.record_filter('image_tag')
      raise ContractError, 'fixture image_tag requires a fixture URL' unless url.is_a?(String) && url.start_with?('/cdn/shop/')
      attrs = options.reject { |key, value| key == 'widths' || value.nil? }
      source = %(src="#{CGI.escapeHTML(url)}")
      if options['widths']
        widths = options['widths'].split(',').map { |width| Integer(width.strip) }
        base = url.sub(/\?width=\d+\z/, '')
        srcset = widths.map { |width| "#{base}?width=#{width} #{width}w" }.join(', ')
        source += %( srcset="#{CGI.escapeHTML(srcset)}")
      end
      attrs.sort.each do |key, value|
        raise ContractError, "invalid fixture image attribute #{key}" unless /\A[\w-]+\z/.match?(key)
        source += %( #{key}="#{CGI.escapeHTML(value.to_s)}")
      end
      "<img #{source}>"
    end

    def money(value)
      host.record_filter('money')
      host.money(value)
    end

    def money_with_currency(value)
      host.record_filter('money_with_currency')
      "#{host.money(value)} USD"
    end

    def json(value)
      host.record_filter('json')
      JSON.generate(value)
    end

    def inline_asset_content(name)
      host.record_filter('inline_asset_content')
      host.asset_content(name)
    end

    def asset_url(name)
      host.record_filter('asset_url')
      host.asset_content(name)
      "/assets/#{name}"
    end

    def standard_event_data(resource, event, options = {})
      host.record_filter('standard_event_data')
      raise ContractError, 'fixture supports view product/cart events only' unless resource.is_a?(Hash) && event == 'view'
      payload = if resource.key?('id')
        { 'event' => event, 'product_id' => resource.fetch('id'), 'context' => options['context'] }
      elsif resource.key?('items') && resource.key?('total_price')
        { 'event' => event, 'cart_item_count' => resource.fetch('item_count'), 'cart_total_price' => resource.fetch('total_price'), 'context' => options['context'] }
      else
        raise ContractError, 'fixture event resource must be a product or cart'
      end
      JSON.generate(payload)
    end

    def md5(value)
      host.record_filter('md5')
      Digest::MD5.hexdigest(value.to_s)
    end

    def preload_tag(url, options = {})
      host.record_filter('preload_tag')
      attrs = options.sort.map { |key, value| %( #{key}="#{CGI.escapeHTML(value.to_s)}") }.join
      %(<link rel="preload" href="#{CGI.escapeHTML(url.to_s)}"#{attrs}>)
    end

    def stylesheet_tag(url, options = {})
      host.record_filter('stylesheet_tag')
      raise ContractError, 'unsupported fixture stylesheet_tag options' unless (options.keys - ['preload']).empty?
      preload = options['preload'] ? preload_tag(url, 'as' => 'style') : ''
      %(#{preload}<link rel="stylesheet" href="#{CGI.escapeHTML(url.to_s)}">)
    end

    def font_modify(font, property, value)
      host.record_filter('font_modify')
      raise ContractError, 'font_modify expects a fixture font' unless font.is_a?(Font)
      case property
      when 'weight'
        weight = value == 'bold' ? 700 : Integer(value)
        Font.new(font.to_s, weight: weight, style: font.style)
      when 'style'
        raise ContractError, "unsupported font style #{value}" unless %w[normal italic].include?(value)
        Font.new(font.to_s, weight: font.weight, style: value)
      else
        raise ContractError, "unsupported font property #{property}"
      end
    end

    def font_face(font, options = {})
      host.record_filter('font_face')
      raise ContractError, 'fixture font_face expects system font and font_display' unless font.is_a?(Font) && (options.keys - ['font_display']).empty?
      ''
    end

    def date(value, format)
      if %w[now today].include?(value)
        host.record_filter('date:fixture_clock')
        value = host.fixture.fetch('manifest').fetch('created_at')
      end
      super(value, format)
    end

    def link_to(text, url)
      host.record_filter('link_to')
      %(<a href="#{CGI.escapeHTML(url.to_s)}">#{CGI.escapeHTML(text.to_s)}</a>)
    end

    def item_count_for_variant(cart, variant_id)
      host.record_filter('item_count_for_variant')
      raise ContractError, 'fixture variant count requires a cart object' unless cart.is_a?(Hash) && cart['items'].is_a?(Array)
      cart['items'].sum do |item|
        item.fetch('variant_id', item.dig('variant', 'id')) == variant_id ? item.fetch('quantity') : 0
      end
    end

    def payment_terms(form)
      host.record_filter('payment_terms')
      unless form.is_a?(Hash) && form['type'] == 'cart' && host.fixture.dig('manifest', 'platform_capabilities', 'payment_terms') == false
        raise ContractError, 'fixture payment_terms requires explicitly disabled cart financing'
      end
      ''
    end

    def color_brightness(value)
      host.record_filter('color_brightness')
      color = value.is_a?(Color) ? value : Color.new(value)
      (color.red * 299 + color.green * 587 + color.blue * 114) / 1000.0
    end

    def color_contrast(first, second)
      host.record_filter('color_contrast')
      luminances = [first, second].map do |value|
        color = value.is_a?(Color) ? value : Color.new(value)
        channels = [color.red, color.green, color.blue].map do |channel|
          channel /= 255.0
          channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055)**2.4
        end
        channels.zip([0.2126, 0.7152, 0.0722]).sum { |channel, weight| channel * weight }
      end
      low, high = luminances.minmax
      ((high + 0.05) / (low + 0.05)).round(1)
    end

    def color_lighten(value, amount)
      host.record_filter('color_lighten')
      raise ContractError, 'fixture lightening must be between 0 and 100' unless (0..100).cover?(amount)
      (value.is_a?(Color) ? value : Color.new(value)).shifted_lightness(amount)
    end

    def color_darken(value, amount)
      host.record_filter('color_darken')
      raise ContractError, 'fixture darkening must be between 0 and 100' unless (0..100).cover?(amount)
      (value.is_a?(Color) ? value : Color.new(value)).shifted_lightness(-amount)
    end

    def color_modify(value, property, amount)
      host.record_filter('color_modify')
      raise ContractError, 'fixture color_modify supports alpha 0..1 only' unless property == 'alpha' && (0..1).cover?(amount)
      color = value.is_a?(Color) ? value : Color.new(value)
      "rgba(#{color.red}, #{color.green}, #{color.blue}, #{amount})"
    end

    def placeholder_svg_tag(identifier, css_class = '')
      host.record_filter('placeholder_svg_tag')
      raise ContractError, "unsupported placeholder #{identifier}" unless identifier == 'hero-apparel-1'
      %(<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 800" class="#{CGI.escapeHTML(css_class.to_s)}" role="img" aria-label="Fixture placeholder"><rect width="1200" height="800" fill="#e8e8e8"/></svg>)
    end

    private

    def host = @context.registers[:horizon]
  end

  class Renderer
    CSS_SENTINEL = '<!-- horizon-fixture-stylesheets -->'
    attr_reader :fixture, :stylesheets, :platform_filters, :rendered_sources

    def initialize(theme_root:, fixture:)
      @source = Source.new(theme_root)
      @fixture = fixture
      raise ContractError, 'expected synthetic fixture schema version 1' unless fixture['synthetic'] == true && fixture['schema_version'] == 1
      @environment = Liquid::Environment.build(error_mode: :strict, file_system: @source) do |env|
        env.register_filter(Filters)
        env.register_tag('schema', SchemaTag)
        env.register_tag('stylesheet', StylesheetTag)
        env.register_tag('style', StyleTag)
        env.register_tag('content_for', ContentForTag)
        env.register_tag('sections', SectionsTag)
        env.register_tag('paginate', PaginateTag)
        env.register_tag('render', RenderTag)
        env.register_tag('form', FormTag)
      end
      @cache = {}
      reset
    end

    def render(page: 'index', scope: 'hero')
      reset
      raise ContractError, "unsupported scope #{scope.inspect}" unless %w[hero template page].include?(scope)
      @globals = typed_globals(deep_copy(fixture.fetch('globals')))
      @globals.dig('cart', 'items')&.each_with_index { |item, index| item['index'] = index }
      page_data = fixture.fetch('pages').fetch(page)
      @globals.merge!('page_title' => page_data.fetch('title'), 'page_description' => page_data.fetch('description'), 'canonical_url' => @globals.fetch('canonical_url', page_data.fetch('url')), 'current_page' => @globals.fetch('current_page', 1), 'current_tags' => @globals.fetch('current_tags', []))
      @globals['template'] = TemplateName.new(page)
      @globals['settings'] = global_settings
      template_data = @source.json(page_data.fetch('template'))
      ids = scope == 'hero' ? template_data.fetch('order').first(1) : template_data.fetch('order')
      body = ids.each_with_index.map { |id, index| render_section(id, template_data.fetch('sections').fetch(id), index + 1) }.join
      if scope == 'page'
        body = render_path('layout/theme.liquid', @globals.merge('content_for_layout' => body, 'content_for_header' => CSS_SENTINEL))
        raise ContractError, 'page layout must expose content_for_header once' unless body.scan(CSS_SENTINEL).size == 1
        body = body.sub(CSS_SENTINEL, %(<style data-horizon-fixture>#{stylesheets.values.join}</style>))
      elsif !%w[hero template].include?(scope)
        raise ContractError, "unsupported scope #{scope.inspect}"
      end
      body
    end

    def render_path(path, globals, locals = {}, parent: nil)
      record_source(path)
      template = (@cache[path] ||= Liquid::Template.new(environment: @environment).parse(@source.read(path), error_mode: :strict, line_numbers: true))
      context = Context.build(environment: @environment, static_environments: globals, outer_scope: locals, registers: { horizon: self, file_system: @source }, rethrow_errors: true, resource_limits: parent&.resource_limits)
      context.template_name = path
      context.strict_filters = true
      context.strict_variables = false
      template.render!(context)
    end

    def render_content_for(context, kind, args)
      parent = context['block'] || context['section']
      nodes = parent.fetch('blocks')
      globals = context.static_environments.first
      closest = (globals['closest'] || {}).merge(args.filter_map { |key, value| [key.delete_prefix('closest.'), value] if key.start_with?('closest.') }.to_h)
      locals = args.reject { |key, _| %w[type id].include?(key) || key.start_with?('closest.') }
      case kind
      when 'blocks'
        parent.fetch('block_order').filter_map do |id|
          node = nodes.fetch(id)
          render_block(id, node, globals, closest, locals, context) unless node['static']
        end.join
      when 'block'
        id, type = args.fetch('id'), args.fetch('type')
        node = nodes.fetch(id) { { 'type' => type, 'settings' => {}, 'blocks' => {} } }
        raise ContractError, "block #{id} type mismatch" unless node.fetch('type') == type
        render_block(id, node, globals, closest, locals, context)
      else
        raise ContractError, "unsupported content_for #{kind.inspect}"
      end
    end

    def render_group(name)
      data = @source.json("sections/#{name}.json")
      data.fetch('order').each_with_index.map { |id, index| render_section(id, data.fetch('sections').fetch(id), index + 1) }.join
    end

    def collect_stylesheet(name, body)
      name = "snippets/#{name}.liquid" unless name.end_with?('.liquid')
      @stylesheets[name] ||= body
    end

    def record_source(path)
      @rendered_sources << path unless @rendered_sources.include?(path)
    end

    def record_filter(name)
      @platform_filters << name unless @platform_filters.include?(name)
    end

    def translate(key, options)
      text = key.split('.').reduce(@source.json('locales/en.default.json')) { |value, part| value.fetch(part) }
      text = text.fetch(options['count'] == 1 ? 'one' : 'other') if text.is_a?(Hash) && options.key?('count')
      raise ContractError, "fixture translation #{key} requires scalar text" unless text.is_a?(String)
      text.gsub(/{{\s*(\w+)\s*}}/) { |placeholder| options.fetch(Regexp.last_match(1), placeholder).to_s }
    rescue KeyError => error
      raise ContractError, "fixture translation #{key}: #{error.message}"
    end

    def money(value)
      raise ContractError, 'fixture money supports USD only' unless @globals.dig('shop', 'currency') == 'USD'
      cents = value.nil? ? 0 : Integer(value)
      sign = cents.negative? ? '-' : ''
      dollars, remainder = cents.abs.divmod(100)
      grouped = dollars.to_s.reverse.scan(/.{1,3}/).join(',').reverse
      "#{sign}$#{grouped}.#{format('%02d', remainder)}"
    end

    def asset_content(name)
      raise ContractError, "invalid asset name #{name.inspect}" unless /\A[\w.-]+\z/.match?(name)
      @source.read("assets/#{name}")
    end

    def manifest
      { 'synthetic' => true, 'parser_mode' => 'strict', 'render' => 'render!', 'strict_variables' => false, 'strict_filters' => true, 'theme_sha' => fixture.dig('theme', 'sha'), 'liquid_version' => Liquid::VERSION, 'sources' => rendered_sources, 'platform_filters' => platform_filters, 'stylesheets' => stylesheets.keys,
        'platform_contract' => { 'pagination' => 'first page only', 'font' => 'configured Inter uses local system Arial', 'events' => 'synthetic product/cart view JSON', 'form_submission' => 'unsupported', 'cart_item_index' => 'derived zero-based index', 'payment_terms' => 'empty only when fixture explicitly disables service', 'optional_variables' => 'missing optional properties resolve to nil', 'clock' => fixture.dig('manifest', 'created_at') } }
    end

    private

    def reset
      @stylesheets, @platform_filters, @rendered_sources = {}, [], []
    end

    def deep_copy(value) = Marshal.load(Marshal.dump(value))

    def typed_globals(value)
      case value
      when Array then value.map { |item| typed_globals(item) }
      when Hash
        value.to_h do |key, item|
          [key, key == 'focal_point' && item.is_a?(Hash) ? FocalPoint.new(item) : typed_globals(item)]
        end
      else value
      end
    end

    def global_settings
      schema = @source.json('config/settings_schema.json').flat_map { |group| group.fetch('settings', []) }
      defaults = setting_defaults(schema)
      configured = defaults.merge(@source.json('config/settings_data.json').fetch('current')).merge(fixture.fetch('theme').fetch('settings', {}))
      materialize_settings(configured, schema, @globals)
    end

    def setting_defaults(definitions)
      definitions.each_with_object({}) do |definition, values|
        next unless definition['id']
        values[definition['id']] = if definition.key?('default')
          definition['default']
        elsif definition['type'] == 'checkbox'
          false
        elsif %w[select radio].include?(definition['type'])
          definition.fetch('options').first.fetch('value')
        else
          nil
        end
      end
    end

    def materialize_settings(values, definitions, globals)
      bindings = globals.key?('settings') ? globals : globals.merge('settings' => values)
      values = values.transform_values { |value| binding_value(value, bindings) }
      definitions.each do |definition|
        id = definition['id']
        next unless id && values[id] && values[id] != ''
        values[id] = case definition['type']
        when 'color' then Color.new(values[id])
        when 'color_palette' then Palette.new(values[id])
        when 'font_picker' then Font.new(values[id])
        when 'collection' then globals.fetch('collections', {})[values[id]]
        when 'product' then globals.fetch('all_products', {})[values[id]]
        when 'link_list' then globals.fetch('linklists', {})[values[id]]
        when 'url' then values[id].to_s.sub('shopify://', '/')
        else values[id]
        end
      end
      values
    end

    def binding_value(value, globals)
      return value unless value.is_a?(String) && value.include?('{{')
      match = value.match(/\A{{\s*([\w.]+)\s*}}\z/)
      lookup = ->(path) { path.split('.').reduce(globals) { |object, key| object.respond_to?(:[]) ? object[key] : nil } }
      return lookup.call(match[1]) if match
      rendered = value.gsub(/{{\s*([\w.]+)\s*}}/) { lookup.call(Regexp.last_match(1)).to_s }
      raise ContractError, "unsupported fixture setting binding #{value}" if rendered.include?('{{')
      rendered
    end

    def render_section(id, raw_node, index)
      node = deep_copy(raw_node)
      node['settings'] = node.fetch('settings', {}).merge(fixture.dig('theme', 'section_overrides', id, 'settings') || {})
      path = "sections/#{node.fetch('type')}.liquid"
      schema = @source.schema(path)
      node['id'], node['index'] = id, index
      node['blocks'] ||= {}
      node['block_order'] ||= []
      node['blocks'] = BlockCollection.new(node['blocks'], node['block_order'])
      definitions = schema.fetch('settings', [])
      node['settings'] = materialize_settings(setting_defaults(definitions).merge(node['settings']), definitions, @globals)
      closest = node['settings'].key?('collection') ? { 'collection' => node['settings']['collection'] } : {}
      body = render_path(path, @globals.merge('section' => node, 'block' => nil, 'closest' => closest))
      wrap(schema.fetch('tag', 'div'), "shopify-section-#{id}", ['shopify-section', schema['class']].compact.join(' '), body)
    end

    def render_block(id, raw_node, globals, closest, locals, parent)
      node = deep_copy(raw_node)
      path = "blocks/#{node.fetch('type')}.liquid"
      schema = @source.schema(path)
      node['settings'] = node.fetch('settings', {}).merge(fixture.dig('theme', 'block_overrides', id, 'settings') || {})
      node['id'], node['shopify_attributes'] = id, ''
      node['blocks'] ||= {}
      node['block_order'] ||= []
      node['blocks'] = BlockCollection.new(node['blocks'], node['block_order'])
      definitions = schema.fetch('settings', [])
      node['settings'] = materialize_settings(setting_defaults(definitions).merge(node['settings']), definitions, globals.merge('closest' => closest))
      body = render_path(path, globals.merge('block' => node, 'closest' => closest), locals, parent: parent)
      wrap(schema.fetch('tag', 'div'), "shopify-block-#{id}", ['shopify-block', schema['class']].compact.join(' '), body)
    end

    def wrap(tag, id, css_class, body)
      return body if tag.nil?
      raise ContractError, "invalid wrapper tag #{tag}" unless /\A[a-z][a-z0-9-]*\z/.match?(tag)
      %(<#{tag} id="#{CGI.escapeHTML(id)}" class="#{CGI.escapeHTML(css_class)}">#{body}</#{tag}>)
    end
  end
end
