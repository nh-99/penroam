// StoreKit 2 for Penroam's subscriptions. Reachable only from the app's Rust side (see the
// plugin's `lib.rs`).
//
// This file never finishes a transaction on its own. Finishing tells Apple the purchase has been
// delivered, and that's true only once the sync server has it, so `finish` runs when Rust says so.
// An unfinished transaction comes back through `Transaction.unfinished`, which is what makes a crash
// or an outage during a purchase safe.
#if canImport(UIKit)
import StoreKit
import SwiftRs
import Tauri
import UIKit
import WebKit

struct ProductsArgs: Decodable {
    let ids: [String]
}

struct PurchaseArgs: Decodable {
    let productId: String
    let accountToken: String
}

struct FinishArgs: Decodable {
    let transaction: String
}

struct ProductInfo: Encodable {
    let id: String
    let displayPrice: String
    let period: String?
}

struct ProductList: Encodable {
    let products: [ProductInfo]
}

struct PurchaseAnswer: Encodable {
    let status: String
    let transaction: String?
}

struct TransactionList: Encodable {
    let transactions: [String]
}

final class StoreBillingPlugin: Plugin {
    private var updates: Task<Void, Never>?

    override func load(webview: WKWebView) {
        // StoreKit expects a listener from launch. Transactions that arrive here stay unfinished,
        // and the app picks them up through `unfinished` when it next comes to the foreground.
        updates = Task.detached {
            for await _ in Transaction.updates {}
        }
    }

    deinit {
        updates?.cancel()
    }

    private static func period(_ product: Product) -> String? {
        guard let p = product.subscription?.subscriptionPeriod, p.value == 1 else { return nil }
        switch p.unit {
        case .day: return "day"
        case .week: return "week"
        case .month: return "month"
        case .year: return "year"
        @unknown default: return nil
        }
    }

    /// Transactions whose signature StoreKit verified. The server checks with Apple anyway, so an
    /// unverified one is only skipped, not treated as an attack.
    private static func verified(_ source: Transaction.Transactions) async -> [Transaction] {
        var found: [Transaction] = []
        for await result in source {
            if case .verified(let transaction) = result,
                transaction.productType == .autoRenewable
            {
                found.append(transaction)
            }
        }
        return found
    }

    @objc public func products(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(ProductsArgs.self)
        Task {
            do {
                let found = try await Product.products(for: args.ids)
                invoke.resolve(
                    ProductList(
                        products: found.map {
                            ProductInfo(
                                id: $0.id, displayPrice: $0.displayPrice,
                                period: Self.period($0))
                        }))
            } catch {
                invoke.reject(error.localizedDescription)
            }
        }
    }

    @objc public func purchase(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(PurchaseArgs.self)
        Task {
            do {
                guard let product = try await Product.products(for: [args.productId]).first else {
                    invoke.reject("This plan isn't available in the App Store right now.")
                    return
                }
                guard let token = UUID(uuidString: args.accountToken) else {
                    invoke.reject("This account can't buy a plan right now.")
                    return
                }
                switch try await product.purchase(options: [.appAccountToken(token)]) {
                case .success(.verified(let transaction)):
                    invoke.resolve(
                        PurchaseAnswer(status: "purchased", transaction: String(transaction.id)))
                case .success(.unverified):
                    invoke.reject("The App Store couldn't confirm this purchase.")
                case .pending:
                    invoke.resolve(PurchaseAnswer(status: "pending", transaction: nil))
                case .userCancelled:
                    invoke.resolve(PurchaseAnswer(status: "cancelled", transaction: nil))
                @unknown default:
                    invoke.reject("The App Store gave an unexpected answer.")
                }
            } catch {
                invoke.reject(error.localizedDescription)
            }
        }
    }

    @objc public func unfinished(_ invoke: Invoke) throws {
        Task {
            let found = await Self.verified(Transaction.unfinished)
            invoke.resolve(TransactionList(transactions: found.map { String($0.id) }))
        }
    }

    @objc public func restore(_ invoke: Invoke) throws {
        Task {
            do {
                try await AppStore.sync()
                let found = await Self.verified(Transaction.currentEntitlements)
                invoke.resolve(TransactionList(transactions: found.map { String($0.id) }))
            } catch {
                invoke.reject(error.localizedDescription)
            }
        }
    }

    @objc public func finish(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(FinishArgs.self)
        Task {
            for transaction in await Self.verified(Transaction.unfinished)
            where String(transaction.id) == args.transaction {
                await transaction.finish()
            }
            invoke.resolve()
        }
    }
}

@_cdecl("init_plugin_store_billing")
func initPlugin() -> Plugin {
    return StoreBillingPlugin()
}
#endif
